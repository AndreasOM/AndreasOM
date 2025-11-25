// graphql-client generate --schema-path ./graphql/github.schema.graphql --custom-scalars-module crate::gql_types --output-directory ./src/ --response-derives Debug ./graphql/github_queries.graphql
mod github_queries;
pub(crate) mod gql_types {
    #[allow(clippy::upper_case_acronyms)]
    pub(crate) type URI = String;
    pub(crate) type DateTime = String; //chrono::DateTime<chrono::Utc>;
}

use anyhow::Result;
use chrono::{DateTime, Datelike, Utc};
use github_queries::{
    issues_and_prs_query, user_repos_query, IssuesAndPrsQuery, UserReposQuery,
};
use human_bytes::human_bytes;
use itertools::Itertools;
use once_cell::sync::Lazy;
use reqwest::Client;
use serde::Serialize;
use serde_json;
use std::{cmp::Ordering, collections::HashMap, env, fs::File, io::Write, path::PathBuf};
use tinytemplate::TinyTemplate;

const VERSION: &str = env!("CARGO_PKG_VERSION");

const MY_LOGIN: &str = "AndreasOM";

// Repository listing configuration
const TOP_STARRED_REPOS: usize = 5;
const TOP_FORKED_REPOS: usize = 5;
const TOP_RECENT_REPOS: usize = 10;

// Language statistics configuration
const MIN_LANGUAGE_PERCENTAGE: f64 = 1.0;

// API rate limiting configuration
const PAGINATION_DELAY_MS: u64 = 200;

#[derive(Debug, Serialize)]
struct MyRepo {
    full_name: String,
    url: String,
    fork_count: i64,
    stargazer_count: i64,
    pushed_date: String,
}

#[derive(Debug, Default, Serialize)]
struct UserAndRepoStats {
    created_at: String,
    total_repos: i64,
    owned_repos: i64,
    forked_repos: i64,
    live_repos: i64,
    all_time_languages: HashMap<String, (String, i64)>,
    recent_languages: HashMap<String, (String, i64)>,
    repos: Vec<MyRepo>,
}

#[derive(Debug, Serialize)]
struct TopRepos<'a> {
    most_recent: Vec<&'a MyRepo>,
    most_starred: Vec<&'a MyRepo>,
    most_forked: Vec<&'a MyRepo>,
}

#[derive(Debug, Serialize)]
struct LanguageStat<'a> {
    name: &'a str,
    color: &'a str,
    percentage: i64,
    bytes: String,
}

#[derive(Debug, Default, Serialize)]
struct IssueAndPrStats {
    issues_created: i64,
    issues_closed: i64,
    prs_created: i64,
    prs_merged: i64,
}

#[derive(Serialize)]
struct Context<'a> {
    user_and_repo_stats: &'a UserAndRepoStats,
    top_repos: TopRepos<'a>,
    issue_and_pr_stats: IssueAndPrStats,
    top_all_time_languages: Vec<LanguageStat<'a>>,
    top_recent_languages: Vec<LanguageStat<'a>>,
}

const README_TEMPLATE: &str = include_str!("../README_TEMPLATE.md");

/*
## GitHub Activity Stats
- {issue_and_pr_stats.prs_created} PRs created
  - of which {issue_and_pr_stats.prs_merged} were merged
- {issue_and_pr_stats.issues_created} issues created
  - of which {issue_and_pr_stats.issues_closed} have been closed
*/

const API_URL: &str = "https://api.github.com/graphql";

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    let token = env::var("GITHUB_TOKEN")
        .expect("You must set the GITHUB_TOKEN env var when running this program");
    let client = Client::builder()
        .user_agent(format!("andreasOM-profiler-generator/{}", VERSION))
        .default_headers(
            std::iter::once((
                reqwest::header::AUTHORIZATION,
                reqwest::header::HeaderValue::from_str(&format!("Bearer {}", token))
                    .map_err(|e| anyhow::anyhow!("Invalid authorization header: {}", e))?,
            ))
            .collect(),
        )
        .build()?;

    let user_and_repo_stats = user_and_repo_stats(&client).await?;
    tracing::debug!("{user_and_repo_stats:#?}");
    let top_repos = top_repos(&user_and_repo_stats.repos);
    let top_all_time_languages = top_languages(&user_and_repo_stats.all_time_languages);
    tracing::debug!("{top_all_time_languages:#?}");
    let top_recent_languages = top_languages(&user_and_repo_stats.recent_languages);
    tracing::debug!("{top_recent_languages:#?}");
    let issue_and_pr_stats = issue_and_pr_stats(&client).await?;
    tracing::debug!("{issue_and_pr_stats:#?}");

    let mut tt = TinyTemplate::new();
    tt.add_template("readme", README_TEMPLATE)?;
    let context = Context {
        user_and_repo_stats: &user_and_repo_stats,
        top_repos,
        issue_and_pr_stats,
        top_all_time_languages,
        top_recent_languages,
    };

    let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    path.push("README.md");
    let mut file = File::create(path)?;
    file.write_all(tt.render("readme", &context)?.as_bytes())?;

    Ok(())
}

async fn graphql_with_retry<Q: graphql_client::GraphQLQuery>(
    client: &Client,
    url: &str,
    variables: Q::Variables,
) -> Result<graphql_client::Response<Q::ResponseData>>
where
    Q::Variables: serde::Serialize + Clone,
{
    const MAX_RETRIES: u32 = 4;
    const INITIAL_DELAY_MS: u64 = 1000;

    for attempt in 1..=MAX_RETRIES {
        tracing::debug!("GraphQL request attempt {}/{}", attempt, MAX_RETRIES);

        // Build the request body
        let request_body = graphql_client::QueryBody {
            variables: variables.clone(),
            query: Q::build_query(variables.clone()).query,
            operation_name: Q::build_query(variables.clone()).operation_name,
        };

        // Make the HTTP request directly to capture the raw response
        let http_response = client
            .post(url)
            .json(&request_body)
            .send()
            .await;

        match http_response {
            Ok(response) => {
                let status = response.status();
                tracing::debug!("HTTP response status: {}", status);

                // Get the raw response text
                let response_text = match response.text().await {
                    Ok(text) => text,
                    Err(e) => {
                        tracing::error!("Failed to read response body: {}", e);
                        if attempt < MAX_RETRIES {
                            let delay_ms = INITIAL_DELAY_MS * 2u64.pow(attempt - 1);
                            tracing::warn!("Retrying in {}ms...", delay_ms);
                            tokio::time::sleep(tokio::time::Duration::from_millis(delay_ms)).await;
                            continue;
                        } else {
                            return Err(anyhow::anyhow!("Failed to read response body: {}", e));
                        }
                    }
                };

                // Try to parse as GraphQL response
                match serde_json::from_str::<graphql_client::Response<Q::ResponseData>>(&response_text) {
                    Ok(graphql_response) => {
                        tracing::debug!("GraphQL request succeeded on attempt {}", attempt);
                        return Ok(graphql_response);
                    }
                    Err(e) => {
                        tracing::error!("GraphQL request attempt {}/{} failed to parse response", attempt, MAX_RETRIES);
                        tracing::error!("Parse error: {}", e);
                        tracing::error!("HTTP status: {}", status);
                        tracing::error!("Full response body:\n{}", response_text);

                        if attempt < MAX_RETRIES {
                            let delay_ms = INITIAL_DELAY_MS * 2u64.pow(attempt - 1);
                            tracing::warn!("Retrying in {}ms...", delay_ms);
                            tokio::time::sleep(tokio::time::Duration::from_millis(delay_ms)).await;
                        } else {
                            tracing::error!("All {} retry attempts exhausted", MAX_RETRIES);
                            return Err(anyhow::anyhow!(
                                "GraphQL request failed after {} attempts. Last error: {}. HTTP status: {}",
                                MAX_RETRIES,
                                e,
                                status
                            ));
                        }
                    }
                }
            }
            Err(e) => {
                tracing::error!("HTTP request attempt {}/{} failed: {}", attempt, MAX_RETRIES, e);

                if attempt < MAX_RETRIES {
                    let delay_ms = INITIAL_DELAY_MS * 2u64.pow(attempt - 1);
                    tracing::warn!("Retrying in {}ms...", delay_ms);
                    tokio::time::sleep(tokio::time::Duration::from_millis(delay_ms)).await;
                } else {
                    tracing::error!("All {} retry attempts exhausted", MAX_RETRIES);
                    return Err(anyhow::anyhow!("HTTP request failed after {} attempts: {}", MAX_RETRIES, e));
                }
            }
        }
    }

    unreachable!("Loop should always return before reaching here");
}

async fn user_and_repo_stats(client: &Client) -> Result<UserAndRepoStats> {
    let mut stats = UserAndRepoStats::default();
    let mut after = None;
    tracing::info!("Getting user repos for login: {}", MY_LOGIN);
    loop {
        let vars = user_repos_query::Variables {
            login: MY_LOGIN.to_string(),
            after,
        };
        tracing::debug!("Making GraphQL request to {} for user {}", API_URL, MY_LOGIN);
        let resp = graphql_with_retry::<UserReposQuery>(client, API_URL, vars).await?;
        tracing::debug!("{resp:#?}");

        if resp.data.is_none() {
            tracing::error!("No data in GraphQL response. Full response: {:#?}", resp);
        }
        let data = resp.data.ok_or_else(|| anyhow::anyhow!("No data in GraphQL response"))?;

        if data.user.is_none() {
            tracing::error!("No user in GraphQL response");
        }
        let user = data.user.ok_or_else(|| anyhow::anyhow!("No user in GraphQL response"))?;

        if stats.created_at.is_empty() {
            stats.created_at = user.created_at;
        }

        let nodes = user.repositories
                .nodes
                .ok_or_else(|| anyhow::anyhow!("No repository nodes in response"))?;

        let owned_repos: Vec<_> = nodes
                .into_iter()
                .filter_map(|r| r)
                .filter(|r| r.owner.login == MY_LOGIN)
                .collect();

        collect_user_repo_stats(&mut stats, owned_repos)?;

        if user.repositories.page_info.has_next_page {
            after = user.repositories.page_info.end_cursor;
            // Small delay between paginated requests to avoid rate limiting
            tokio::time::sleep(tokio::time::Duration::from_millis(PAGINATION_DELAY_MS)).await;
        } else {
            break;
        }
    }
    Ok(stats)
}

fn collect_user_repo_stats(
    stats: &mut UserAndRepoStats,
    repos: Vec<user_repos_query::ReposNodes>,
) -> Result<()> {
    for repo in repos {
        if repo.is_archived || repo.is_disabled || repo.is_empty || repo.is_private {
            continue;
        }

        stats.total_repos += 1;
        if repo.is_fork {
            stats.forked_repos += 1;
            continue;
        }

        stats.owned_repos += 1;

        let languages = match repo.languages.as_ref() {
            Some(langs) => langs,
            None => continue, // Skip repos with no language data
        };

        let lang_sizes: Vec<_> = languages
            .edges
            .as_ref()
            .map(|edges| {
                edges.iter()
                    .filter_map(|e| e.as_ref().map(|edge| edge.size))
                    .collect()
            })
            .unwrap_or_default();

        let lang_names_and_colors: Vec<_> = languages
            .nodes
            .as_ref()
            .map(|nodes| {
                nodes.iter()
                    .filter_map(|l| l.as_ref().map(|lang| (lang.name.as_str(), lang.color.as_deref())))
                    .collect()
            })
            .unwrap_or_default();

        collect_language_stats(
            &mut stats.all_time_languages,
            repo.name_with_owner.as_str(),
            &lang_sizes,
            &lang_names_and_colors,
        );

        let refs = match repo.refs.as_ref() {
            Some(r) => r,
            None => continue, // Skip repos with no refs
        };

        let nodes = match refs.nodes.as_ref() {
            Some(n) if !n.is_empty() => n,
            _ => continue, // Skip repos with no commits
        };

        let target = match nodes[0].as_ref().and_then(|n| n.target.as_ref()) {
            Some(t) => t,
            None => continue,
        };

        let pushed_date = match target {
            user_repos_query::ReposNodesRefsNodesTarget::Commit(c) => match c.pushed_date.as_ref() {
                Some(d) => d,
                None => continue, // Skip commits without pushed_date
            },
            _ => continue,
        };

        let pushed_date = DateTime::parse_from_rfc3339(pushed_date)?.with_timezone(&Utc);
        if pushed_date < *FILTER_DATE {
            continue;
        }

        collect_language_stats(
            &mut stats.recent_languages,
            repo.name_with_owner.as_str(),
            &lang_sizes,
            &lang_names_and_colors,
        );

        stats.live_repos += 1;

        stats.repos.push(MyRepo {
            full_name: repo.name_with_owner,
            url: repo.url,
            fork_count: repo.fork_count,
            stargazer_count: repo.stargazer_count,
            pushed_date: pushed_date.format("%Y-%m-%d").to_string(),
        });
    }

    Ok(())
}

static FILTER_DATE: Lazy<DateTime<Utc>> = Lazy::new(|| {
    let now = chrono::Utc::now();
    // The chrono::Duration struct cannot represent 2 years, only multiple of
    // weeks, but two years is not 104 weeks.  let two_years_ago =
    let two_years_ago = format!("{}-{}", now.year() - 2, now.format("%m-%dT%H:%M:%SZ"),);
    chrono::DateTime::parse_from_rfc3339(&two_years_ago)
        .unwrap_or_else(|_| panic!("Could not parse `{two_years_ago}` as an RFC3339 date"))
        .with_timezone(&Utc)
});

const REPOS_TO_IGNORE_FOR_LANGUAGE_STATS: &[&str] = &[
    // The presentations repo has a ton of HTML and JS I didn't write
    // and this distorts the stats.
//    "autarch/presentations",
    // The mason book is HTML, but it's just the HTMl from the old dynamic
    // site which I crawled, so it's not interesting for these stats.
//    "autarch/masonbook.houseabsolute.com",
];

fn collect_language_stats(
    stats: &mut HashMap<String, (String, i64)>,
    repo_name: &str,
    lang_sizes: &[i64],
    lang_names_and_colors: &[(&str, Option<&str>)],
) {
    if lang_sizes.len() != lang_names_and_colors.len() {
        tracing::warn!(
            "language sizes and names differ in length: {} != {} for {}; skipping",
            lang_sizes.len(),
            lang_names_and_colors.len(),
            repo_name,
        );
        return;
    }
    if !lang_sizes.is_empty() && !REPOS_TO_IGNORE_FOR_LANGUAGE_STATS.contains(&repo_name) {
        for i in 0..lang_sizes.len() {
            let lang = match (repo_name, lang_names_and_colors[i].0) {
                // This is really XS, not C (although arguably, XS is just C).
                //("houseabsolute/File-LibMagic", "C") => "XS",
                (_, l) => l,
            };

            // The tidyall repo has a bunch of PHP and JS checked in for
            // testing, but none of it is code I've written or maintained.
            /*
            if repo_name == "houseabsolute/perl-code-tidyall" && lang != "Perl" {
                continue;
            }
            */
            let color = language_color(lang, lang_names_and_colors[i].1);
            let size = lang_sizes[i];
            if let Some(v) = stats.get_mut(lang) {
                (*v).1 += size;
            } else {
                stats.insert(lang.to_string(), (color.to_string(), size));
            }
        }
    }
}

fn language_color<'a, 'b>(lang: &'a str, color: Option<&'b str>) -> &'b str {
    match color {
        Some(c) => c,
        None => match lang {
            "Perl 6" => "#00A9E0",
            "XS" => "#021c9e", // a darker blue than Perl,
            _ => {
                tracing::warn!("No color defined for language '{}'; using default gray", lang);
                "#808080" // Default gray color
            }
        },
    }
}

fn top_repos(repos: &[MyRepo]) -> TopRepos<'_> {
    let most_forked = top_n(repos, TOP_FORKED_REPOS, |a, b| b.fork_count.cmp(&a.fork_count));
    let most_starred = top_n(repos, TOP_STARRED_REPOS, |a, b| b.stargazer_count.cmp(&a.stargazer_count));
    let most_recent = top_n(repos, TOP_RECENT_REPOS, |a, b| b.pushed_date.cmp(&a.pushed_date));
    TopRepos {
        most_forked,
        most_recent,
        most_starred,
    }
}

fn top_n<S>(repos: &[MyRepo], take: usize, sorter: S) -> Vec<&MyRepo>
where
    S: FnMut(&&MyRepo, &&MyRepo) -> Ordering,
{
    repos
        .iter()
        .sorted_by(sorter)
        .take(take)
        .collect::<Vec<_>>()
}

fn top_languages(languages: &HashMap<String, (String, i64)>) -> Vec<LanguageStat<'_>> {
    let total_size: i64 = languages.values().map(|v| v.1).sum();
    let colors: HashMap<&str, &str> = languages
        .iter()
        .map(|(k, v)| (k.as_str(), v.0.as_str()))
        .collect();

    let mut language_sums: HashMap<&str, i64> = HashMap::new();
    for (lang, (_, size)) in languages {
        if let Some(v) = language_sums.get_mut(lang.as_str()) {
            *v += *size;
        } else {
            language_sums.insert(lang, *size);
        }
    }

    let mut top = vec![];
    for (name, sum) in language_sums {
        let pct = (sum as f64 / total_size as f64) * 100.0;
        if pct < MIN_LANGUAGE_PERCENTAGE {
            tracing::debug!("Skipping language {name} with total percentage of {pct}");
            continue;
        }
        let color = colors.get(name).copied().unwrap_or_else(|| {
            tracing::warn!("No color found for language '{}'; using default", name);
            "#808080"
        });
        top.push(LanguageStat {
            name,
            color,
            percentage: pct.round() as i64,
            bytes: human_bytes(sum as f64),
        })
    }

    top.sort_by(|a, b| b.percentage.cmp(&a.percentage));
    top
}

async fn issue_and_pr_stats(client: &Client) -> Result<IssueAndPrStats> {
    tracing::info!("Getting issue and pr data");
    let resp = graphql_with_retry::<IssuesAndPrsQuery>(
        client,
        API_URL,
        issues_and_prs_query::Variables {},
    )
    .await?;
    tracing::debug!("{resp:#?}");

    if resp.data.is_none() {
        tracing::error!("No data in issues/PRs GraphQL response. Full response: {:#?}", resp);
    }
    let data = resp.data.ok_or_else(|| anyhow::anyhow!("No data in issues/PRs GraphQL response"))?;
    Ok(IssueAndPrStats {
        issues_created: data.issues_created.issue_count,
        issues_closed: data.issues_closed.issue_count,
        prs_created: data.prs_created.issue_count,
        prs_merged: data.prs_merged.issue_count,
    })
}


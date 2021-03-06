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
    issues_and_prs_query, organization_repos_query, user_repos_query, IssuesAndPrsQuery,
    OrganizationReposQuery, UserReposQuery,
};
use graphql_client::reqwest::post_graphql;
use human_bytes::human_bytes;
use itertools::Itertools;
use once_cell::sync::Lazy;
use reqwest::Client;
use rss::Channel;
use serde_derive::Serialize;
use std::{cmp::Ordering, collections::HashMap, env, fs::File, io::Write, path::PathBuf};
use tinytemplate::TinyTemplate;

const VERSION: &str = env!("CARGO_PKG_VERSION");

const MY_LOGIN: &str = "AndreasOM";

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

#[derive(Debug, Serialize)]
struct BlogPost {
    title: String,
    date: String,
    url: String,
}

#[derive(Serialize)]
struct Context<'a> {
    user_and_repo_stats: &'a UserAndRepoStats,
    top_repos: TopRepos<'a>,
    issue_and_pr_stats: IssueAndPrStats,
    top_all_time_languages: Vec<LanguageStat<'a>>,
    top_recent_languages: Vec<LanguageStat<'a>>,
    blog_posts: Vec<BlogPost>,
}

const README_TEMPLATE: &str = r#"
# Andreas "anti" Neukoetter

I am Andreas, but friends call me Anti.  
I am a fulltime game developer.  
I currently work as the CTO of ...  
In my day job I mostly spend my time between meetings/calls, and spreadsheets.  
In my spare time I love to code.  

## Repo Stats
- **{user_and_repo_stats.live_repos} repos with commits in the last two years**
- {user_and_repo_stats.total_repos} total repos
  - {user_and_repo_stats.forked_repos} are forks

This excludes archived, disabled, empty, and private repos.

## Repos with Recent Pushes
{{ for repo in top_repos.most_recent }}- [{repo.full_name}]({repo.url}) on {repo.pushed_date}
{{ endfor }}

## Most Starred
{{ for repo in top_repos.most_starred }}- [{repo.full_name}]({repo.url}) - {repo.stargazer_count} stars
{{ endfor }}

## Most Forked
{{ for repo in top_repos.most_forked }}- [{repo.full_name}]({repo.url}) - {repo.fork_count} forks
{{ endfor }}

## Past Two Years Language Stats
{{ for lang in top_recent_languages }}- {lang.name}: {lang.percentage}%, {lang.bytes}
{{ endfor }}

## All-Time Language Stats
{{ for lang in top_all_time_languages }}- {lang.name}: {lang.percentage}%, {lang.bytes}
{{ endfor }}
"#;

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
        .user_agent(format!("autarch-profiler-generator-om/{}", VERSION))
        .default_headers(
            std::iter::once((
                reqwest::header::AUTHORIZATION,
                reqwest::header::HeaderValue::from_str(&format!("Bearer {}", token)).unwrap(),
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
    let blog_posts = Vec::new();
//    let blog_posts = blog_posts().await?;
//    tracing::debug!("{blog_posts:#?}");

    let mut tt = TinyTemplate::new();
    tt.add_template("readme", README_TEMPLATE)?;
    let context = Context {
        user_and_repo_stats: &user_and_repo_stats,
        top_repos,
        issue_and_pr_stats,
        top_all_time_languages,
        top_recent_languages,
        blog_posts,
    };

    let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    path.push("README.md");
    let mut file = File::create(path)?;
    file.write_all(tt.render("readme", &context)?.as_bytes())?;

    Ok(())
}

async fn user_and_repo_stats(client: &Client) -> Result<UserAndRepoStats> {
    let mut stats = UserAndRepoStats::default();
    let mut after = None;
    tracing::info!("Getting user repos");
    loop {
        let vars = user_repos_query::Variables {
            login: MY_LOGIN.to_string(),
            after,
        };
        let resp = post_graphql::<UserReposQuery, _>(client, API_URL, vars).await?;
        tracing::debug!("{resp:#?}");
//        todo!("---");

        let user = resp.data.unwrap().user.unwrap();
        if stats.created_at.is_empty() {
            stats.created_at = user.created_at;
        }

        collect_user_repo_stats(
            &mut stats,
            user.repositories
                .nodes
                .unwrap()
                .into_iter()
                .filter(|r| r.as_ref().unwrap().owner.login == MY_LOGIN)
                .collect::<Vec<_>>(),
        )?;

        if user.repositories.page_info.has_next_page {
            after = user.repositories.page_info.end_cursor;
        } else {
            break;
        }
    }
/*
    after = None;
    tracing::info!("Getting organization repos");
    loop {
        let vars = organization_repos_query::Variables {
            login: "andreasOM".to_string(),
            after,
        };
        let resp = post_graphql::<OrganizationReposQuery, _>(client, API_URL, vars).await?;
        tracing::debug!("{resp:#?}");

        match resp.data.unwrap().organization {
            Some( organization ) => {
                collect_organization_repo_stats(&mut stats, organization.repositories.nodes.unwrap())?;

                if organization.repositories.page_info.has_next_page {
                    after = organization.repositories.page_info.end_cursor;
                } else {
                    break;
                }
            },
            None => {},
        }
    }
*/
    Ok(stats)
}

fn collect_user_repo_stats(
    stats: &mut UserAndRepoStats,
    repos: Vec<Option<user_repos_query::ReposNodes>>,
) -> Result<()> {
    for repo in repos.into_iter().map(|r| r.unwrap()) {
        if repo.is_archived || repo.is_disabled || repo.is_empty || repo.is_private {
            continue;
        }

        stats.total_repos += 1;
        if repo.is_fork {
            stats.forked_repos += 1;
            continue;
        }

        stats.owned_repos += 1;

        let lang_sizes = repo
            .languages
            .as_ref()
            .unwrap()
            .edges
            .as_ref()
            .unwrap()
            .iter()
            .map(|e| e.as_ref().unwrap().size)
            .collect::<Vec<_>>();
        let lang_names_and_colors = repo
            .languages
            .as_ref()
            .unwrap()
            .nodes
            .as_ref()
            .unwrap()
            .iter()
            .map(|l| {
                let l = l.as_ref().unwrap();
                (l.name.as_str(), l.color.as_deref())
            })
            .collect::<Vec<_>>();
        collect_language_stats(
            &mut stats.all_time_languages,
            repo.name_with_owner.as_str(),
            &lang_sizes,
            &lang_names_and_colors,
        );

        let last_commit = repo.refs.as_ref().unwrap().nodes.as_ref().unwrap()[0]
            .as_ref()
            .unwrap()
            .target
            .as_ref()
            .unwrap();
        let pushed_date = match last_commit {
            user_repos_query::ReposNodesRefsNodesTarget::Commit(c) => c.pushed_date.as_ref(),
            _ => None,
        };
        // This seems to be none in cases where the last commit in the repo is
        // from before I moved to GitHub, which means the repo is not live,
        // since I'm pretty sure there's nothing I've moved to GitHub in the
        // last 2 years or less.
        if pushed_date.is_none() {
            continue;
        }

        let pushed_date = DateTime::parse_from_rfc3339(pushed_date.unwrap())?.with_timezone(&Utc);
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

// It's gross that this is a near copy of the user stats fn, but there are
// only two ways to avoid that which I can think of, and the both suck.
//
// One way is to have the two ReposNodes structs implement a common trait, and
// then have a single fn that accepts Vec<Option<impl NodeTrait>>. But that
// means that all data in the repo is gated behind functions, and it makes
// moving data out of the repo more complicated.
//
// The other way is to implement some sort of macro that generates two copies
// of this fn.
fn collect_organization_repo_stats(
    stats: &mut UserAndRepoStats,
    repos: Vec<Option<organization_repos_query::ReposNodes>>,
) -> Result<()> {
    for repo in repos.into_iter().map(|r| r.unwrap()) {
        if repo.is_archived || repo.is_disabled || repo.is_empty || repo.is_private {
            continue;
        }

        stats.total_repos += 1;
        if repo.is_fork {
            stats.forked_repos += 1;
            continue;
        }

        stats.owned_repos += 1;

        let lang_sizes = repo
            .languages
            .as_ref()
            .unwrap()
            .edges
            .as_ref()
            .unwrap()
            .iter()
            .map(|e| e.as_ref().unwrap().size)
            .collect::<Vec<_>>();
        let lang_names_and_colors = repo
            .languages
            .as_ref()
            .unwrap()
            .nodes
            .as_ref()
            .unwrap()
            .iter()
            .map(|l| {
                let l = l.as_ref().unwrap();
                (l.name.as_str(), l.color.as_deref())
            })
            .collect::<Vec<_>>();
        collect_language_stats(
            &mut stats.all_time_languages,
            repo.name_with_owner.as_str(),
            &lang_sizes,
            &lang_names_and_colors,
        );

        let last_commit = repo.refs.as_ref().unwrap().nodes.as_ref().unwrap()[0]
            .as_ref()
            .unwrap()
            .target
            .as_ref()
            .unwrap();
        let pushed_date = match last_commit {
            organization_repos_query::ReposNodesRefsNodesTarget::Commit(c) => {
                c.pushed_date.as_ref()
            }
            _ => None,
        };
        // This seems to be none in cases where the last commit in the repo is
        // from before I moved to GitHub, which means the repo is not live,
        // since I'm pretty sure there's nothing I've moved to GitHub in the
        // last 2 years or less.
        if pushed_date.is_none() {
            continue;
        }

        let pushed_date =
            DateTime::parse_from_rfc3339(pushed_date.as_ref().unwrap())?.with_timezone(&Utc);
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
        panic!(
            "language sizes and names differ in length: {} != {} for {}",
            lang_sizes.len(),
            lang_names_and_colors.len(),
            repo_name,
        );
    }
    if !lang_sizes.is_empty() && !REPOS_TO_IGNORE_FOR_LANGUAGE_STATS.contains(&repo_name) {
        for i in 0..lang_sizes.len() - 1 {
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
            _ => panic!("No color for {lang}"),
        },
    }
}

fn top_repos(repos: &[MyRepo]) -> TopRepos<'_> {
    let most_forked = top_n(repos, 5, |a, b| b.fork_count.cmp(&a.fork_count));
    let most_starred = top_n(repos, 5, |a, b| b.stargazer_count.cmp(&a.stargazer_count));
    let most_recent = top_n(repos, 10, |a, b| b.pushed_date.cmp(&a.pushed_date));
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

fn top_languages(languages: &HashMap<String, (String, i64)>) -> Vec<LanguageStat> {
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
        if pct < 1.0 {
            tracing::debug!("Skipping language {name} with total percentage of {pct}");
            continue;
        }
        top.push(LanguageStat {
            name,
            color: *colors.get(name).unwrap(),
            percentage: pct.round() as i64,
            bytes: human_bytes(sum as f64),
        })
    }

    top.sort_by(|a, b| b.percentage.cmp(&a.percentage));
    top
}

async fn issue_and_pr_stats(client: &Client) -> Result<IssueAndPrStats> {
    tracing::info!("Getting issue and pr data");
    let resp =
        post_graphql::<IssuesAndPrsQuery, _>(client, API_URL, issues_and_prs_query::Variables {})
            .await?;
    tracing::debug!("{resp:#?}");
//    todo!("---");

    let data = resp.data.unwrap();
    Ok(IssueAndPrStats {
        issues_created: data.issues_created.issue_count,
        issues_closed: data.issues_closed.issue_count,
        prs_created: data.prs_created.issue_count,
        prs_merged: data.prs_merged.issue_count,
    })
}

async fn blog_posts() -> Result<Vec<BlogPost>> {
    tracing::info!("Getting blog feed");
    let content = reqwest::get("https://blog.urth.org/index.xml")
        .await?
        .bytes()
        .await?;
    let mut channel = Channel::read_from(&content[..])?;
    Ok(channel
        .items
        .splice(0..5, None)
        .into_iter()
        .map(|i| {
            let dt = DateTime::parse_from_rfc2822(i.pub_date().unwrap())?;
            Ok(BlogPost {
                title: i.title.unwrap(),
                date: dt.date().format("%Y-%m-%d").to_string(),
                url: i.link.unwrap(),
            })
        })
        .collect::<Result<Vec<_>>>()?)
}

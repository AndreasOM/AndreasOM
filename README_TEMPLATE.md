# Andreas "anti" Neukoetter

I am Andreas, but friends call me Anti.
I am a fulltime game developer.
I currently work as the Head of Development of NARC,
and do some freelance consulting.
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

{{ if top_repos.most_forked }}
## Most Forked
{{ for repo in top_repos.most_forked }}- [{repo.full_name}]({repo.url}) - {repo.fork_count} forks
{{ endfor }}

{{ endif }}
## Past Two Years Language Stats
{{ for lang in top_recent_languages }}- {lang.name}: {lang.percentage}%, {lang.bytes}
{{ endfor }}

## All-Time Language Stats
{{ for lang in top_all_time_languages }}- {lang.name}: {lang.percentage}%, {lang.bytes}
{{ endfor }}

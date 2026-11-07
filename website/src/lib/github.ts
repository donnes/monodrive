// The repo's star count, read from GitHub when the site builds so the number
// shown is the real one. One request per build or dev server, since the
// unauthenticated API allows 60 an hour. A failed request shows plain "GitHub".

const REPO = "donnes/monodrive";

let stars: Promise<number | null> | undefined;

export const githubStars = () =>
  (stars ??= fetch(`https://api.github.com/repos/${REPO}`, {
    headers: {
      Accept: "application/vnd.github+json",
      // CI runners share IPs, so the build there passes its token to stay under the limit.
      ...(import.meta.env.GITHUB_TOKEN && { Authorization: `Bearer ${import.meta.env.GITHUB_TOKEN}` }),
    },
  })
    .then((res) => (res.ok ? res.json() : null))
    .then((repo: { stargazers_count?: unknown } | null) =>
      typeof repo?.stargazers_count === "number" ? repo.stargazers_count : null,
    )
    .catch(() => null));

/** 4, 980, 1.2k, 24k. */
export const formatStars = (n: number) =>
  n < 1000 ? String(n) : `${(n / 1000).toFixed(n < 10_000 ? 1 : 0).replace(/\.0$/, "")}k`;

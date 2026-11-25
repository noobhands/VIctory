# Issue tracker

| Setting | Value |
|---|---|
| Tracker | GitHub issues on the user's fork, `noobhands/VIctory` (remote `fork`) |
| Upstream | `Marvisak/VIctory` (remote `origin`): **never** file issues or PRs there from this workflow |
| Publish mode | local-first: specs and tickets draft into `docs/features/<slug>/`; nothing reaches the tracker until the user says "publish" |
| Chosen | 2026-11-24, by the user |

## Mechanics

`gh` resolves this clone to the upstream repo by default, so every command
names the fork explicitly:

```sh
gh issue create -R noobhands/VIctory --title "<title>" --body-file docs/features/<slug>/tickets/T01-<slug>.md
gh issue comment -R noobhands/VIctory <n> --body-file <file>
gh issue close   -R noobhands/VIctory <n>
```

Bodies are always passed by file (`--body-file`) from the feature dossier,
never retyped. After publishing, rename the ticket file to `GH-<n>.md` and
record the number in `04-tickets.md`.

Status lives in labels: `status:todo`, `status:in-progress`, `status:done`,
`status:reopened`. Blocking is written in each issue body ("Blocked by #n").

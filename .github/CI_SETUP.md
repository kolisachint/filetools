# Activating CI

The CI workflow lives at `.github/ci.yml` instead of `.github/workflows/ci.yml`
because the token used by the automated session lacks the GitHub `workflow`
scope and cannot push files under `.github/workflows/`.

To activate it, move the file into place and push from a credential that has the
`workflow` scope (e.g. your normal `git` login):

    git mv .github/ci.yml .github/workflows/ci.yml
    git rm .github/CI_SETUP.md
    git commit -m "Activate CI workflow"
    git push

It runs `cargo fmt --check`, `cargo clippy` (warnings denied), and `cargo test`
on pushes to `main` and on pull requests — all currently green locally.

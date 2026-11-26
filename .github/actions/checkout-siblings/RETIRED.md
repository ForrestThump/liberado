# Retired: checkout-siblings

This composite action is retired. Liberado pins `turbovault*` and `turbomcp*` as
Cargo git+rev dependencies on the public ForrestThump forks (one commit SHA per
fork). CI no longer checks those repositories out as
path siblings; Cargo fetches the revisions itself.

Do not restore `action.yml` or re-add a `uses: ./.github/actions/checkout-siblings`
step. Leftover `turbovault/` and `turbomcp/` directories are gitignored and are
not required to build.

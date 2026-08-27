# Retired: checkout-siblings

This composite action is retired. Liberado pins `turbovault*` and `turbomcp*` as
Cargo git+tag dependencies on the public ForrestThump forks
(`tag = "liberado-2026-08-27"`). CI no longer checks those repositories out as
path siblings; Cargo fetches the tags itself.

Do not restore `action.yml` or re-add a `uses: ./.github/actions/checkout-siblings`
step. Leftover `turbovault/` and `turbomcp/` directories are gitignored and are
not required to build.

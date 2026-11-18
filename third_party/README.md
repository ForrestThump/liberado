# Vendored forks

These trees are the ForrestThump tags Liberado used to fetch by git:

- `turbovault/` is tag `liberado-2026-11-10`
- `turbomcp/` is tag `liberado-2026-08-27`

They are path dependencies so the workspace can drop `chrono` and `chrono-tz`.
Time types in the compiled crates use `jiff`. `schemars` no longer enables the
`chrono04` feature. Do not add those crates back.

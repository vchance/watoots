# watoots-cli

The `watoots` command line, for the
[watoots](https://crates.io/crates/watoots) plugin host.

```console
$ watoots inspect decoder.wasm -m policy.toml     # what can this plugin do?
$ watoots run decoder.wasm -m policy.toml -c sniff -- '[113, 111, 105, 102]'
$ watoots record decoder.wasm -m policy.toml -c decode -o bug.wave -- "$bytes"
$ watoots replay bug.wave -c decoder.wasm --assert
$ watoots diff deployed.wasm candidate.wasm -m policy.toml
```

- `inspect` is a capability summary -- what the plugin wants, what the policy
  grants, and which imports your application is expected to serve -- not an
  import list, though `--imports` gives you that too.
- `run` calls one export with WAVE arguments, under the policy.
- `record` and `replay` are the record/replay half of the project: a failed
  call's trace is written anyway, because that is the recording worth keeping,
  and `replay --emit-test` turns one into a Rust regression test.
- `diff` previews what `reload` would refuse: a plugin update that wants more
  than the policy grants, or drops an export a caller uses. Non-zero exit, so
  it works as a gate.
- `fuzz` generates type-correct calls from the world's own types and uses
  replay as the oracle; `wit semver-check` is `wasm-tools component
  semver-check`, linked rather than reimplemented.

Install with `cargo install watoots-cli`. The repository's
[README](https://github.com/vchance/watoots#readme) walks through all of it
on a real file-format decoder.

Licensed under Apache-2.0 WITH LLVM-exception.

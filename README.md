# Downlint

Rust implementation of a Markdown checker and LSP inspired by Marksman.

## Configuration

Downlint reads a `.downlint.toml` at the workspace root. All sections are optional;
unspecified keys fall back to defaults.

### `[wiki]`

| Key | Type | Default | Description |
|---|---|---|---|
| `obsidian_prefix` | bool | `false` | When `true`, wiki-link targets resolve against leading prefixes of file stems. For example, `[[20260801-topic-a]]` matches `20260801-topic-a-sub-x.md`. When `false` (default), a wiki-link target must equal a file stem (modulo existing title-slug and relative-path matching). When `false` and a broken wiki-link target is a leading prefix of one or more file stems, the `DNL002` diagnostic gains a hint listing the candidates so the user can discover the option. Multi-match cases (the prefix matches more than one file) emit `DNL001` ambiguous-link diagnostics, regardless of the flag value. |

Example:

```toml
[wiki]
obsidian_prefix = true
```
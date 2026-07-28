<img src=https://github.com/DrCMWither/mate/blob/master/assets/logo/mate.svg width=300 />

# mate

`mate` is a safe meta package manager based on Rust. It discovers supported package-manager instances in trusted system locations and `PATH`, searches them with bounded parallelism, asks the user to choose both a manager and an install target, shows the complete plan, and starts install after confirmation.

> [!IMPORTANT]
> `mate` is currently a prototype, please review every plan before confirming it, especially when on a instance you care about.

## Why mate?

Installing the same tool can mean `apt-get install`, `brew install`, `cargo install`, `npm install`, `pip install`, or `uv tool install`. `mate` provides one workflow while retaining each manager's native package identity and install model:

1. Discover available manager instances and context-aware targets.
2. Search supported registries with bounded parallelism.
3. Select a verified package, manager instance, and target.
4. Inspect the exact commands, working directories, environment changes, privileges, and filesystem guards.
5. Confirm, then run the plan serially.

## Supported managers

| Manager | Search source | Supported targets | install behaviour |
| --- | --- | --- | --- |
| APT | Configured APT repos | `system` | `apt-get install -- ...` via trusted `sudo` |
| pacman | Configured pacman repos | `system` | `pacman -S --needed -- ...` via trusted `sudo` |
| Homebrew | Homebrew formulae | `user` | `brew install --formula ...` |
| Cargo | [crates.io](https://crates.io) | Cargo install roots | One pinned `cargo install --locked` step per crate |
| npm | Public npm registry | Detected project, workspace, or global prefix | Pinned install with lifecycle scripts disabled |
| pip | Public PyPI | `user`, or the environment owning a discovered venv-bound `pip` | One pinned, batched `pip install` |
| uv | Public PyPI | `user` or Python virtual environment | `uv tool install`, or create/use a venv and run `uv pip install` |

Python searches currently perform exact distribution lookups on public PyPI. Private indexes, version constraints in queries, and fuzzy Python package search are **not** supported.


## Build

Use the current stable Rust toolchain:

```sh
cargo build
cargo test
```

Run the development binary:

```sh
cargo run -- doctor
```

## Usage

Discover manager instances and targets:

```sh
mate doctor
mate doctor --json
```

Search all discovered managers:

```sh
mate search ripgrep
mate search requests --manager pip,uv
```

Interactively select a manager and target:

```sh
mate install ripgrep requests
```

Preview an explicit non-interactive plan:

```sh
mate install requests rich \
  --manager uv \
  --target "venv:$PWD/.venv" \
  --dry-run \
  --yes
```

> NOTE: To prevent typosquatting attacks, flag `--yes` does not yes during the fuzzy searches, regardless of wheather the `instance` param is included.

After reviewing the dry run, omit `--dry-run` to execute that same explicit selection:

```sh
mate install requests rich \
  --manager uv \
  --target "venv:$PWD/.venv" \
  --yes
```

For Homebrew:

```sh
mate install ripgrep jq --manager brew --target user --dry-run --yes
```

For APT:

```sh
mate install ripgrep jq --manager apt --target system --dry-run --yes
```

## Development

The core types are:

- `ManagerAdapter`: discovery, search, compatible targets, and plan generation.
- `ManagerInstance`: a concrete executable, including venv bound pip instances.
- `Target`: system, user, an existing Python venv, or a new project `.venv`.
- `Candidate`: a manager-native package identity and search score.
- `InstallPlan`: selected candidates plus exact command specifications.

Adapters only produce structured `CommandSpec` values. The executor is the sole component allowed to launch an install command.

```sh
cargo fmt --check
cargo test
cargo clippy --all-targets --all-features -- -D warnings
```

Source layout:

```text
src/
├── adapters/   Package-manager discovery, search, targets, and plan generation
├── cli.rs      Command-line contract
├── context.rs  Project markers and target discovery
├── engine.rs   Doctor, search, and install orchestration
├── matching.rs Candidate normalization, ranking, and fuzzy fallback
├── model.rs    Shared domain types
├── planner.rs  Selection grouping and command-plan construction
├── process.rs  Bounded probes and confirmed command execution
├── platform.rs Platform-specific path and ownership handling
└── ui.rs       Rendering, interactive selection, and confirmation
```

## License

[MIT](LICENSE)
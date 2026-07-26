<img src=https://github.com/DrCMWither/mate/blob/master/src/assets/logo/mate.svg width=300 />

# mate

`mate` is a safe meta package manager based on Rust. It discovers supported package-manager instances in trusted system locations and `PATH`, searches them with bounded parallelism, asks the user to choose both a manager and an install target, shows the complete plan, and starts install after confirmation.

This project supports:

- APT (`apt-get` + `apt-cache`)
- pacman
- Homebrew
- pip
- uv

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

After reviewing the dry run, omit `--dry-run` to execute that same explicit
selection:

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

## Target behaviour

| Manager | Target | Planned behaviour |
| --- | --- | --- |
| apt | `system` | `apt-get install -- ...` |
| pacman | `system` | `pacman -S --needed -- ...` |
| brew formula | `user` | `brew install --formula ...` |
| pip outside venv | `user` | `pip install --user ...` |
| pip from an explicitly active external venv | matching `venv:/...` | `pip install ...` |
| uv | `user` | one `uv tool install ...` per package |
| uv | existing/new `venv:/...` | `uv venv` if needed, then one batched `uv pip install` |

`pip` and `uv` currently use an exact public-PyPI JSON lookup and install from that same explicitly selected source. Private Python indexes and fuzzy search are intentionally unsupported.

A batch accepts at most 64 unique queries. Search jobs are limited to 8 in flight; installation groups execute serially and stop on the first failure.

## Architecture

The core types are:

- `ManagerAdapter`: discovery, search, compatible targets, and plan generation.
- `ManagerInstance`: a concrete executable, including venv bound pip instances.
- `Target`: system, user, an existing Python venv, or a new project `.venv`.
- `Candidate`: a manager-native package identity and search score.
- `InstallPlan`: selected candidates plus exact command specifications.

Adapters only produce structured `CommandSpec` values. The executor is the sole component allowed to launch an install command.

Source layout:

```text
src/
  adapters/    apt, pacman, brew, pip, uv
  cli.rs       command-line contract
  context.rs   project markers and target discovery
  engine.rs    doctor/search/install orchestration
  model.rs     normalized domain types
  planner.rs   batch grouping and plan construction
  process.rs   bounded read-only probes and confirmed execution
  ui.rs        rendering, selection, and confirmation
```
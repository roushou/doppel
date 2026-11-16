<div align="center">

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="docs/wordmark-dark.svg">
  <img src="docs/wordmark-light.svg" alt="Doppel" height="60">
</picture>

<h3>Isolated macOS machines for AI agents</h3>

Fork a Mac, hand it to an agent over MCP, watch what it does, take over,
and roll back.

[![CI](https://github.com/roushou/doppel/actions/workflows/check.yml/badge.svg)](https://github.com/roushou/doppel/actions/workflows/check.yml)
[![License: MIT or Apache-2.0](https://img.shields.io/badge/license-MIT%20%2F%20Apache--2.0-blue)](#license)

[Get started](#get-started) · [MCP tools](docs/mcp.md) ·
[Roadmap](docs/roadmap.md) · [How it works](docs/doppeld.md)

</div>

<p align="center">
  <img
    src="https://github.com/user-attachments/assets/5f16e1ee-f0f7-4acb-805e-32a656aef77a"
    alt="The browser viewer: an agent's mark over a note in a macOS VM, beside the accessibility tree it acts on"
    width="800"
  >
</p>

Doppel runs macOS VMs on your Mac and serves them to agents over MCP. One
daemon owns every machine, so agents, viewers and recordings all see the
same state.

It's early: the [roadmap](docs/roadmap.md) says what works.

## What it does

- **Machines:** create from a restore image with no person, fork from
  checkpoints, roll back. Golden images make forks start agent-ready in
  seconds.
- **Agent tools over MCP:** accessibility snapshots with refs, acting by ref
  or point, typing, recognized text (OCR), waiting on evidence, `exec`, and
  files.
- **A daemon** (`doppeld`) owns your machines: agents' calls queue per
  machine, and every session is listed, pausable and recordable.
- **Viewers,** desktop and browser: the screen live with what agents do drawn
  over it, take control and hand back, and replay recordings beside their
  traces.
- **Remote:** another Mac's machines over SSH, or the browser viewer over TLS
  with tokens.

## Screenshots

### Browser

<details>
  <summary>
    Screen: the machine live, with what the agent does drawn over it
  </summary>
  <br>
  <img
    src="https://github.com/user-attachments/assets/5ef81db6-d11d-4d4e-86ee-40cb46118fef"
    alt="The browser viewer showing a machine live, with an agent's mark over a note"
  >
</details>

<details>
  <summary>
    Elements: the accessibility tree the agent saw, refs and all
  </summary>
  <br>
  <img
    src="https://github.com/user-attachments/assets/5f16e1ee-f0f7-4acb-805e-32a656aef77a"
    alt="The browser viewer's Elements view, with the accessibility tree beside the screen"
  >
</details>

<details>
  <summary>Text: what text recognition reads on screen</summary>
  <br>
  <img
    src="https://github.com/user-attachments/assets/1becd9b3-b546-434e-8727-b804624f234b"
    alt="The browser viewer's Text view, with recognized text outlined on the screen"
  >
</details>

<details>
  <summary>Replay: a recording beside its trace of calls</summary>
  <br>
  <img
    src="https://github.com/user-attachments/assets/9033cc98-5574-4d1c-9b05-4b2099bfbbe5"
    alt="The browser viewer replaying a recording beside its trace"
  >
</details>

### Desktop

<details>
  <summary>Screen: the machine live, with the agents watching it</summary>
  <br>
  <img
    src="https://github.com/user-attachments/assets/393655d4-19d8-4be3-8e47-6f1cc29e3662"
    alt="The desktop viewer showing a machine live, with an agent's mark over a note"
  >
</details>

## Get started

You need:

- An Apple silicon Mac on macOS 27, with Xcode 27
- Rust (pinned in `rust-toolchain.toml`), Node.js with pnpm, and
  [`buf`](https://buf.build/docs/installation)
- A macOS restore image (`.ipsw`)
- An Apple Development signing identity (recommended: the guest's permission
  grants follow the agent's signature)

Then:

- Build, and make a machine:

  ```sh
  scripts/build.sh
  export PATH="$PWD/target/debug:$PATH"
  doppel daemon install
  doppel create dev --ipsw path/to/macOS.ipsw --user doppel
  doppel agent bootstrap dev --user doppel
  ```

- Grant the agent Accessibility and Screen & System Audio Recording: open
  `doppel view dev`, take control, and allow both in the guest's System
  Settings. The account's password is in
  `~/Library/Application Support/Doppel/machines/dev/login-password`.

- Make it a golden image, fork it, and hand the fork to an agent:

  ```sh
  doppel golden prepare dev
  doppel fork dev ready work
  doppel start work
  claude mcp add doppel -- "$PWD/target/debug/doppel" mcp --machine work
  ```

- `doppel --help` lists every command.

## Documentation

- [MCP](docs/mcp.md): the agent's tools and how they fit together
- [doppeld](docs/doppeld.md): the daemon, sessions, recordings and remote
  access
- [Crates](docs/crates.md): how the code is cut and layered
- [Roadmap](docs/roadmap.md): what works and what's next
- [Security](SECURITY.md): reporting a vulnerability, and what each part
  exposes
- [CLAUDE.md](CLAUDE.md): guidance for agents working on Doppel

## Development

```sh
scripts/check.sh
```

Every change passes it: formatting, `buf lint`, clippy with warnings denied,
and the Rust, Swift and web tests. Golden fixtures in `fixtures/` pin what
other programs read; regenerate them with `DOPPEL_UPDATE_GOLDEN=1` after an
intended change.

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT) at your option. Unless you explicitly state
otherwise, any contribution intentionally submitted for inclusion in Doppel
by you, as defined in the Apache-2.0 license, shall be dual licensed as
above, without any additional terms or conditions.

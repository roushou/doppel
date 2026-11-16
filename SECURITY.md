# Security

## Reporting a vulnerability

Please report vulnerabilities privately, through GitHub's private
vulnerability reporting on this repository (Security → Report a
vulnerability), not in a public issue. Say what you found, how to reproduce
it, and what it lets an attacker do. You'll get an answer once it's read;
fixes are made on `main`.

## What Doppel exposes

Doppel is early software. Know what each part listens on and trusts:

- **doppeld** listens on a unix socket in your data directory, mode 0600:
  only processes running as you can use it, and anything that can use it can
  drive every machine you own.
- **The browser viewer's listener** is off until `doppel daemon web on`.
  When on, it listens on every network address, over TLS with a self-signed
  certificate (`doppel daemon web status` prints its fingerprint), and lets in
  only holders of a token from `doppel daemon token`. A token is printed
  once, expires (12 hours by default, 30 days at most), and can be revoked
  with `doppel daemon revoke`; the daemon keeps only its SHA-256. Anyone with
  a token can watch and drive your machines. `doppel daemon web off` stops
  listening and closes every browser's connection.
- **MCP clients** (`doppel mcp`) can do anything the tools allow on the
  machines they name: run commands, read and write files, type and click.
  Give an agent a machine you're willing to let it change, and keep a
  checkpoint to roll back to.
- **Guests** are macOS VMs under Apple's Virtualization framework. Each
  machine's login password is stored in its bundle, readable only by you.
  A guest can reach your network through NAT unless it was made with
  `doppel create --offline`.

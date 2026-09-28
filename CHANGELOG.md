# Changelog

All notable changes to this project are documented in this file.

This project follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.0] - 2026-09-25

### Added

- `yuge-hasi devkit`, a macOS command-line workflow for pairing with a SteamOS
  developer device, validating a Linux x86_64 game build, transferring it, and
  registering it with Steam.
- Project configuration, named device profiles, host-key verification, and
  pre-deployment checks.
- Codex Skill and Plugin guidance for safe local CLI use.

### Security

- SSH host keys require on-device fingerprint verification.
- Deployment requires an explicit confirmation that the target game is stopped.
- The CLI keeps its authentication material separate from the user's SSH
  configuration.

[Unreleased]: https://github.com/karad/yuge-hasi-dev/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/karad/yuge-hasi-dev/releases/tag/v0.1.0

---
name: yuge-hasi-devkit
description: Operate yuge-hasi-devkit when a user asks to pair with, inspect, validate, or deploy a game build to a SteamOS developer device. Do not use for general explanation or unrelated development work.
---

# Yuge Hasi Devkit

Use the local `yuge-hasi devkit` CLI to transfer an unpacked Linux x86_64 game
build or an Android ARM64 APK from macOS to a compatible SteamOS developer
device and register it with Steam. PocketPop APK transfer, launch, input, exit,
and relaunch after a Steam Frame reboot were verified; other Android titles
remain unverified.

## CLI availability

- Before running a Devkit command, verify that `yuge-hasi` is available on the
  local `PATH`.
- If it is unavailable, do not attempt pairing, validation, or deployment.
  Tell the user to install the released CLI with `brew install karad/yuge-hasi`,
  then ask them to retry after installation completes.

## Safety boundaries

- Do not expose SSH private keys, authentication directories, device connection
  details, or command output containing them in conversation, logs, or public
  artifacts.
- The user must compare the SSH host-key fingerprint with the value displayed on
  the SteamOS device. Do not infer, approve, or substitute that value.
- Pairing must be approved by the user on the SteamOS device. Do not claim that
  an HTTP response proves pairing or SSH authentication succeeded.
- `deploy` changes game files and Steam registration. Before it, ask the user to
  identify the project and device and to confirm that the target game is stopped.

## Command selection

- Use `project init` to create a new owner-only project file from build metadata.
  It validates the build and does not connect to a device. New project files
  contain game settings only (`schema_version = 2`).
  For an APK, pass `--runtime android --source /path/to/Game.apk` and omit
  `--executable`; use a separate Frame project to preserve Deck settings.
- The default Mac identity and trusted host keys are shared across projects in
  `~/.yuge-hasi/devkit-client-rust/`; named devices are shared in
  `~/.yuge-hasi/devices.toml`. Do not assume each project has a separate key or
  device list. `--config` only overrides standalone commands, not project or
  device workflows.
- If an existing project uses `schema_version = 1`, guide the user to run
  `yuge-hasi devkit --project <file> project migrate` before using the new
  default `pair` command. Migration preserves the old credentials, imports its
  devices, and stops on identity or device conflicts. Do not silently generate
  a new key or overwrite shared settings.
- Use `device add` to register a named SteamOS device in the shared device
  list. It guides a terminal pairing flow and saves the device only after
  authentication succeeds.
- Use `status` to read SSH and Steam readiness for a host and login. It does not
  modify the device.
- Use `deploy --device <name> --check` to validate the configured project and
  inspect the selected device without transferring or registering a build.
- Use `deploy --device <name> --game-stopped` only after the explicit safety
  confirmation above. It transfers the build and changes the device's Steam
  registration.

## Output and failures

The CLI emits JSON for command results and structured failures for project,
device, and deployment workflows. Report only the relevant status and failure
stage. Redact credentials, host keys, and private device details before sharing
output.

If a host key is unknown or changed, stop and have the user verify it on the
device. If pairing was not approved, Steam is not ready, or the game is still
running, explain the condition and wait for the user to resolve it. Do not
retry destructive deployment automatically.

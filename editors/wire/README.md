# TinyStore wire schema

Syntax highlighting for the `.wire` files in `protocol/`, the schemas the wire
protocol is written from. One TextMate grammar,
`syntaxes/wire.tmLanguage.json`, serves VS Code and RustRover (IntelliJ).

It only colours the text. The parser in `crates/protocol` is what checks a
schema.

## VS Code

Run the commands from the repository root. Then reload VS Code with
**Developer: Reload Window** from the command palette.

Link the folder, so it stays in step with the repository. On Windows,
PowerShell (creating a link needs Developer Mode or an administrator shell):

    New-Item -ItemType SymbolicLink -Path "$env:USERPROFILE\.vscode\extensions\tinystore-wire" -Target (Resolve-Path editors\wire).Path

Or copy the folder, on Windows in PowerShell:

    Copy-Item -Recurse editors\wire "$env:USERPROFILE\.vscode\extensions\tinystore-wire"

On macOS and Linux, link or copy it into `~/.vscode/extensions/tinystore-wire`:

    ln -s "$PWD/editors/wire" ~/.vscode/extensions/tinystore-wire

Or pack it and install the package:

    cd editors/wire
    npx @vscode/vsce package --no-dependencies --allow-missing-repository --skip-license
    code --install-extension tinystore-wire-0.0.1.vsix

The `.vsix` file is written into `editors/wire`, whose `.gitignore` keeps it
out of the repository.

When the grammar changes, a link needs only **Developer: Reload Window**; a
copy or a package needs copying or installing again first.

## RustRover and IntelliJ

1. Open Settings (Ctrl+Alt+S) and go to Editor → TextMate Bundles.
2. Click `+`, choose the `editors/wire` folder of the repository, and click OK.
3. Open a file in `protocol/`.

RustRover reads a bundle as it starts: after the grammar changes, restart it,
or remove the bundle and add it again.

## Snippets

In VS Code, type `message`, `field`, `read`, `write`, `download`, `exchange`
or `handover`, then press Tab. RustRover takes the highlighting from this folder,
but its snippets come from Live Templates (Settings → Editor → Live
Templates), which this folder does not set up.

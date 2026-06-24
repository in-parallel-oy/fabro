# fabro

> AI-powered workflow orchestration platform. Workflows are defined as Graphviz graphs and executed by the Fabro engine.

I want you to install Fabro for me. Execute the steps below autonomously.

OBJECTIVE: Install the Fabro binary, then start the server so the user can finish setup in a web wizard, then restart the server in configured mode.

DONE WHEN: The Fabro server has been restarted after the wizard and is running in configured (non-install) mode.

## 1. Install the binary

Check whether Homebrew is available:

```bash
command -v brew
```

If `brew` is present, install via Homebrew (preferred — brew manages updates automatically):

```bash
brew install in-parallel-oy/tap/fabro-nightly
```

Otherwise, use the install script:

```bash
curl -fsSL https://raw.githubusercontent.com/in-parallel-oy/fabro/main/apps/marketing/public/install.sh | bash
```

## 2. Verify the binary

```bash
fabro --version
```

This must print a version string like `fabro 0.x.y` before continuing.

## 3. Start the server to open the install wizard

Start the server in the foreground and capture its output:

```bash
fabro server start
```

The server will:

- Detect that Fabro is unconfigured and enter **install mode**
- Print a URL that looks like `http://127.0.0.1:<port>/install?token=…`
- Attempt to open that URL in the user's default browser

Your job:

- Read the server's stderr and find the `http://…/install?token=…` line
- Open that URL in the user's default browser yourself as a safety net (the server also tries). Use `open` on macOS, `xdg-open` on Linux, or `start` on Windows. Duplicate opens are harmless.
- Tell the user: "The Fabro install wizard is open in your browser. Complete it there; I'll restart the server when you're done."
- Keep the `fabro server start` process running and watch for it to exit. The server exits on its own a moment after the user finishes the wizard.

## 4. Restart the server in configured mode

When the `fabro server start` process exits, start it again:

```bash
fabro server start
```

This time it will boot in configured (non-install) mode.

## 5. Confirm completion

Verify the server is running and no longer in install mode:

```bash
curl -fsS http://127.0.0.1:<port>/health
```

The response should **not** contain `"mode": "install"`. If it does, the wizard wasn't completed — tell the user to finish it and wait for the server to restart again.

EXECUTE NOW: Complete steps 1–5. You are done when the restarted server reports a non-install health status.

# OMGB Mobile

A Capacitor + React mobile remote control for [Grok Build](https://github.com/xai-org/grok-build) via the Agent Client Protocol (ACP).

## Features

- QR code pairing to a local `omgb serve` instance.
- Real-time chat with streaming agent responses.
- Tool-call approval cards.
- Slash command palette (`/model`, `/yolo`, `/clear`, `/loop`, `/plan`, `/help`).
- Terminal, diff, and image output rendering.
- Model picker and BYOK provider settings.
- Connection and provider persistence.

## Install

```bash
npm install
```

## Development

```bash
npm run dev     # Vite dev server
npm run build   # Production build -> dist/
npm run typecheck
```

## Capacitor

```bash
npx cap sync
npx cap run android
npx cap run ios
```

## Pairing

On your laptop run `omgb serve` and scan the QR code with the app. Alternatively type the displayed WebSocket URL manually.

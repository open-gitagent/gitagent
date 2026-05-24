# Contributing to GitAgent

Thanks for your interest in contributing to GitAgent! Here's how to get started.

## Getting Started

1. Fork the repository
2. Clone your fork:
   ```bash
   git clone https://github.com/<your-username>/gitagent.git
   cd gitagent
   ```
3. Install dependencies:
   ```bash
   npm install
   ```
4. Build the project:
   ```bash
   npm run build
   ```
5. Run tests:
   ```bash
   npm test
   ```

## Development Workflow

1. Create a feature branch from `main`:
   ```bash
   git checkout -b feat/my-feature
   ```
2. Make your changes in `src/`
3. Run `npm run build` to verify compilation
4. Run `npm test` to ensure tests pass
5. Commit your changes with a clear message
6. Push and open a pull request

## Project Structure

```
src/
├── index.ts          # CLI entry point
├── sdk.ts            # Core SDK (query function)
├── exports.ts        # Public API surface
├── loader.ts         # Agent config loader
├── tools/            # Built-in tools (cli, read, write, memory)
├── voice/            # Voice mode (OpenAI Realtime adapter)
├── hooks.ts          # Script-based hooks
├── sdk-hooks.ts      # Programmatic SDK hooks
├── skills.ts         # Skill expansion
├── workflows.ts      # Workflow metadata
├── agents.ts         # Sub-agent metadata
├── compliance.ts     # Compliance validation
└── audit.ts          # Audit logging
```

## Guidelines

- **TypeScript** — all code is written in strict TypeScript
- **ESM** — the project uses ES modules (`"type": "module"`)
- **Keep it simple** — avoid over-engineering; minimal dependencies
- **Test your changes** — add or update tests in `test/` when applicable
- **One concern per PR** — keep pull requests focused and reviewable

## Commit Messages

Use clear, descriptive commit messages:

- `Add voice mode with OpenAI Realtime adapter`
- `Fix memory tool path resolution on Windows`
- `Update SDK query to support abort signals`

## Reporting Issues

- Search existing issues before opening a new one
- Include reproduction steps, expected vs actual behavior, and your environment (Node version, OS)

## Code of Conduct

Be respectful and constructive. We're all here to build something useful together.

## License

By contributing, you agree that your contributions will be licensed under the [MIT License](./LICENSE).

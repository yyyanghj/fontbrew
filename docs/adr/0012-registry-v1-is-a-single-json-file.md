# Registry v1 is a single JSON file

Fontbrew's first-party registry v1 uses a single remote `registry.json` file as the curated source of package recipes, stored locally by the CLI as a registry snapshot. A single JSON file keeps registry update, schema validation, local snapshot management, and search simple while the curated registry remains intentionally small; broad discovery is delegated to approved search providers instead of thousands of first-party entries.

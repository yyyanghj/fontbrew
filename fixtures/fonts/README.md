# Font Fixtures

These fixtures are committed so font metadata tests do not require live network access.

| Fixture | Source URL | License | Original filename | Modified |
| --- | --- | --- | --- | --- |
| `Inter-Variable.ttf` | <https://github.com/google/fonts/raw/main/ofl/inter/Inter%5Bopsz%2Cwght%5D.ttf> | SIL Open Font License 1.1, from <https://raw.githubusercontent.com/google/fonts/main/ofl/inter/OFL.txt> | `Inter[opsz,wght].ttf` | No; renamed for stable fixture readability. |
| `SourceCodePro-Regular.otf` | <https://github.com/adobe-fonts/source-code-pro/raw/release/OTF/SourceCodePro-Regular.otf> | SIL Open Font License 1.1, from <https://raw.githubusercontent.com/adobe-fonts/source-code-pro/release/LICENSE.md> | `SourceCodePro-Regular.otf` | No. |
| `SourceCodePro-Regular.ttf` | <https://github.com/adobe-fonts/source-code-pro/raw/release/TTF/SourceCodePro-Regular.ttf> | SIL Open Font License 1.1, from <https://raw.githubusercontent.com/adobe-fonts/source-code-pro/release/LICENSE.md> | `SourceCodePro-Regular.ttf` | No. |
| `SourceCodePro-Bold.ttf` | <https://github.com/adobe-fonts/source-code-pro/raw/release/TTF/SourceCodePro-Bold.ttf> | SIL Open Font License 1.1, from <https://raw.githubusercontent.com/adobe-fonts/source-code-pro/release/LICENSE.md> | `SourceCodePro-Bold.ttf` | No. |
| `SourceCodePro-It.ttf` | <https://github.com/adobe-fonts/source-code-pro/raw/release/TTF/SourceCodePro-It.ttf> | SIL Open Font License 1.1, from <https://raw.githubusercontent.com/adobe-fonts/source-code-pro/release/LICENSE.md> | `SourceCodePro-It.ttf` | No. |
| `SourceCodePro-Collection.ttc` | Generated locally from `SourceCodePro-Regular.ttf` and `SourceCodePro-Bold.ttf` above. | SIL Open Font License 1.1, inherited from the Source Code Pro source fonts. | `SourceCodePro-Regular.ttf` and `SourceCodePro-Bold.ttf` | Yes; generated with `fontTools` 4.60.2 in `target/fonttools-venv` using `fontTools.ttLib.TTCollection`. |

## Verification Use

The MVP verification pass uses these fixtures for offline metadata, archive extraction, local install, manifest, activation, and CLI smoke tests. The fixture binaries were not modified during the final verification pass.

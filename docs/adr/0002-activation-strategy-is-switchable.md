# Activation uses tracked copies

Fontbrew activates installed packages by copying real font files into the Fontbrew-owned activation directory at `~/Library/Fonts/Fontbrew`.

Symlink activation was removed because fonts linked under that directory are not reliably discovered by macOS. The activation strategy is therefore no longer configurable. Schema v1 config files containing the former key remain readable, but the value is ignored and removed on the next config write. Older manifests containing symlink records remain readable only so Fontbrew can validate and safely remove those artifacts. New installs, reinstalls, and updates always create copies. If an operation fails after staging an existing activation artifact, rollback restores that original file or legacy symlink unchanged by renaming it back into place.

Fontbrew manages only activation artifacts recorded in its manifest and does not adopt, overwrite, or remove non-managed fonts outside that tracked artifact boundary.

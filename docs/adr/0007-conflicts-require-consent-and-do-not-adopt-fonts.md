# Conflicts require consent and do not adopt fonts

If installing or activating a package conflicts with existing fonts outside Fontbrew's management boundary, Fontbrew must warn and require explicit user consent before continuing. It may install and activate its own managed copy, but it does not adopt, overwrite, or remove non-managed fonts. This preserves the guarantee that only Fontbrew-managed packages can be updated or removed by Fontbrew.

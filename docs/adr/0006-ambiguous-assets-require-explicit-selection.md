# Ambiguous assets require explicit selection

When a source release contains multiple installable font assets, Fontbrew fails the install and asks the user to choose an asset explicitly. It does not guess among formats or variants because choosing the wrong asset can install a different package family, variant, or file format than the user intended.

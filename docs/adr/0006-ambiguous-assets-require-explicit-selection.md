# Ambiguous assets require explicit selection

When a source release contains multiple installable font assets and no recipe resolves the choice, Fontbrew fails the install and asks the user to choose an asset explicitly. It does not guess among formats or variants because choosing the wrong asset can install a different package family, variant, or file format than the user intended. Recipes can encode the common choices for registry packages, and users can override ambiguity with an explicit asset selector.

# Package boundaries use family name with recipe overrides

Fontbrew manages fonts as packages rather than repositories or loose files. By default, discovered font files are grouped into packages by their family name, but curated recipes can override that boundary when a source publishes multiple user-facing variants in one archive or when multiple related families should be installed as one package. This keeps automatic source handling useful while allowing the registry to model real font distributions accurately.

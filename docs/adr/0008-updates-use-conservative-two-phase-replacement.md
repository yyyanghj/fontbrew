# Updates use conservative two-phase replacement

Fontbrew updates a managed package by downloading and parsing the new version before changing the active installation. It only switches activation artifacts and manifest state after validating that the new files still match the package identity recorded in the manifest; otherwise the update stops and leaves the old version active. This avoids losing or replacing fonts when an upstream release changes archive layout, family names, or variants unexpectedly.

# Manifest records local font state

Fontbrew's manifest records the actual Fontbrew-managed font packages installed on the current machine. It is not a project-level desired-state file or lockfile, because Fontbrew manages the user's system font environment rather than application or repository dependencies. This keeps the MVP focused on safe install, update, list, and remove behavior for the local font library.

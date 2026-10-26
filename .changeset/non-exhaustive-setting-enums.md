---
default: major
---

For apps that embed the notes crates: the setting enums of `markraft-notes` are now non-exhaustive, so a `match` on one needs a wildcard arm.

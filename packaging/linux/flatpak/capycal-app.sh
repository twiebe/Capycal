#!/bin/sh
# Flatpak points XDG_CONFIG_HOME and XDG_STATE_HOME into ~/.var/app, whatever
# the app may read. Point them back, so the app uses the config and state of
# a native install (the manifest shares ~/.config/capycal and
# ~/.local/state/capycal). Flatpak sets HOST_XDG_* only when the host has them.
# The state is always the default path: Flatpak can share no other.
export XDG_CONFIG_HOME="${HOST_XDG_CONFIG_HOME:-$HOME/.config}"
export XDG_STATE_HOME="$HOME/.local/state"
exec capycal-app-bin "$@"

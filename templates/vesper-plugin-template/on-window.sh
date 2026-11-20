#!/bin/sh
# Runs when a window opens. VESPER_APP_ID and VESPER_TITLE say which.
echo "$(date +%T) window opened: $VESPER_APP_ID" >> "${XDG_STATE_HOME:-$HOME/.local/state}/vesper/plugin-hello-events.log"

#!/bin/sh
# Launcher search results: one per line as title|description|command.
name="${VESPER_QUERY:-world}"
echo "Say hello to $name|from the hello plugin|notify-send \"Hello $name\""

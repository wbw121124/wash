#!/bin/sh
export PATH="/c/Windows/system32:/c/Windows:/c/Program Files/dotnet:/c/Program Files/Git/cmd"
exec "/c/Program Files/Git Credential Manager/git-credential-manager.exe" "$@"

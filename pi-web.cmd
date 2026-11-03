@echo off
rem Start the portable TUI host and open Web through the existing /web command.
setlocal DisableDelayedExpansion
"%~dp0pi.cmd" /web %*

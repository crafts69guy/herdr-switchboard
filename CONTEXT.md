# Herdr Switchboard

Herdr Switchboard is a terminal control surface for navigating project contexts, recalling shell
commands, and acting on local listening ports without leaving the originating Herdr pane.

## Product surfaces

**Switchboard**:
The Herdr plugin as a whole, including its central menu, direct actions, shared visual language,
configuration, and state.
_Avoid_: Palette, GHQ switcher

**Central Menu**:
The small popup that routes a user to a picker or utility without nesting Herdr panes.
_Avoid_: Main picker, command palette

**Direct Action**:
A Herdr plugin action that opens one picker or utility without passing through the Central Menu.
_Avoid_: Shortcut, entrypoint

**Projects Picker**:
The navigation surface that searches running agents, open workspaces, GHQ repositories, and linked
worktrees.
_Avoid_: Unified picker, GHQ picker

**Usage Popup**:
The surface that reports how much of each AI subscription is spent, when it resets, and when it
renews.
_Avoid_: Quota picker, billing

**Renewal**:
When a subscription is charged again and its allowance starts over — a property of the plan, not of
a rate-limit window. Shown as the `renews` row on a Usage card, and only when the provider states
it; a date this project computed would be a guess.
_Avoid_: Billing date, expiry, next payment

**Quota Window**:
One rate-limit period of one provider — a five-hour bucket, a weekly bucket — with a used
percentage and the moment it rolls over. A provider may have several; the popup promotes the one
closest to running out.
_Avoid_: Limit, bucket

## Commands

**Command**:
The exact shell input recalled or declared as one searchable item; it may span multiple lines and
includes arguments and shell syntax, not only the executable name.
_Avoid_: Executable, binary, process command

**Command Record**:
A Command together with its source, selection history, recent working directories, and safety
classification. Records with identical Command text are one item.
_Avoid_: History line, execution record

**Preset**:
A user-declared Command with a label and optional working-directory rule that remains available
independently of shell-history retention.
_Avoid_: Template, alias

**Origin Pane**:
The Herdr pane from which Switchboard was opened; it supplies the default working directory and is
the only pane into which a Command may be filled or run.
_Avoid_: Current pane, picker pane

## Ports

**Listener**:
An observed local TCP socket in the operating system's listening state, associated with the process
metadata visible to the current user.
_Avoid_: Open port, connection

**Port Entry**:
The searchable item formed by grouping a process's IPv4 and IPv6 Listeners for the same port while
retaining every bind address in its details.
_Avoid_: Socket, endpoint

**Owner Process**:
The exact PID that owns a Listener and is the only process a stop action may signal.
_Avoid_: Parent process, process group

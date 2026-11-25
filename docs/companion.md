# Product direction: automatic setup, Play, and friends

The missing experience is getting friends from a shared modlist to a functioning
Play button without asking them to install and configure several tools. The application
owns that complete workflow. An AOL/AIM-style social client is a separate application
and repository, `SkyrimCoopChat`, not a tab or mandatory component of the launcher.
Both repositories are separate from SkyrimCoop.

SkyrimCoop can be used alone. The launcher can independently install/share exact
setups and launch them, with no chat dependency. The Steam-independent social app
advertises hosted games, public chat, setup shares and play dates; it hands Prepare,
Play and Join requests to optional local applications. It does not implement the
game's connection transport or install mods. A received advertisement is discovery
information, not permission to install or proof of compatibility.

## First release workflow

1. A host shares a pinned supported Wabbajack recipe and launch configuration.
2. A friend opens it in the native desktop application.
3. The application detects Steam and Skyrim, confirms missing requirements, obtains a
   tested installer engine and guides any required account sign-in/download actions.
4. It installs into a dedicated location, retains downloads for retry and performs
   the supported list's post-install configuration, including the matching SKSE and
   Proton/prefix/settings setup on Linux.
5. It verifies installation state. Play always selects this exact managed instance.
6. SkyrimCoop handles joining and verifies the actual loaded installation.

Start with one tested list/runtime combination and expand the supported set after
real acceptance tests. A recipe's successful reconstruction is not enough to certify
its launch. Arbitrary list-specific post-install instructions cannot all be assumed
automatable. No mod organizer is required for the initial supported materialized layout.

The application must not ask a friend to browse for an installer executable, a Proton
directory or a Steam Linux Runtime directory during normal supported setup. Discovery,
version selection and installation are application responsibilities. When detection is
ambiguous, present the concrete choices. Required account/ownership and manual-download
actions remain visible rather than being treated as generic failed installs.

## Reuse before forking

The default is a native wrapper around a versioned installer adapter. Installation,
verification and launching remain accessible through a CLI. The desktop interface
consumes those operations and owns recovery and user-facing state.

Jackify is the nearest existing Linux reference: it already orchestrates Wabbajack
installation, Proton configuration and Steam integration. Inspect/reuse a tested engine
and the relevant setup logic before implementing replacements. Its CLI and engine
have different argument/progress contracts from upstream Wabbajack. The prototype
now downloads/checks release 0.5.10 and has an adapter matching that release's observed
help output. This is not yet a passed real-modlist installation test.

A fork is justified by a demonstrated missing engine capability, such as an unavailable
headless operation or actionable authentication/progress event. Prefer a small engine
patch to adopting the entire Wabbajack desktop application's maintenance surface.
A Jackify-derived application is another option if its existing workflow is a better
starting point than a standalone adapter. This choice needs a real installation probe.

Inspection found Jackify's engine availability/download helpers, engine-specific
invocation adapter, manual-download handling, and separate post-install configuration
steps. CLF3 is also exposed as an engine choice, but its own README states that its
BSA/DDS output can differ from Wabbajack. It is not established as satisfying this
prototype's exact expected-byte contract.

## Friends and play dates

The proposed desktop experience includes a compact buddy list, online/away/playing
status, direct and small group chats, and session cards. A session card contains a
title, time/time zone, pinned setup identity, invitation, RSVP and each participant's
setup readiness. Changing the required setup creates an explicit new version instead
of silently changing what an already accepted event requires.

A friend can choose Prepare now before the play date, allowing lengthy downloads to
finish in advance. At session time, Play/Join resolves the same pinned setup. Chat
works while the game is closed or the installer is busy; neither depends on an active
SkyrimCoop game session. Local scheduling can export calendar events and show reminders.

Social transport is a separate adapter. P2P connectivity, authentication, discovery
and relay availability need a concrete supported design before implementation. A peer
cannot deliver a message/update to an offline recipient without retaining it until
reconnection or using an online mailbox. Initially, persist locally, synchronize with
online peers, and state that delivery is pending; an optional online mailbox can follow.
Reliable offline delivery and notifications are separate features, not implied by P2P.

The social application must have a Steam-independent identity and transport. It does
not own game connections: an opaque invitation is handed to the launcher or SkyrimCoop
after a user action. Public discovery needs a supported bootstrap/relay strategy and
authenticated expiring advertisements; the transport has not been implemented. The
game's own transport may still use Steam, independently of social discovery.

## Acceptance and current state

The first acceptance test is a fresh friend environment reaching a real SKSE game boot
with no manual tool-path configuration, selecting the intended installation, then
joining the host and passing the loaded compatibility check. Also test interrupted
downloads, authentication pauses, manual downloads, missing game/content versions,
and switching back to personal Skyrim without altering it.

The existing prototype contributes isolated staging, manifests, verification, retry,
a native UI and a launch adapter. Pinned Linux engine bootstrap and a versioned,
read-only status interface are implemented. The separate chat repo can query that
interface without importing launcher code. End-to-end supported-pack configuration,
automatic runtime discovery, authentication, social networking, scheduling and
Prepare/Play/Join handoffs remain unfinished. Installation/launch tests still use
synthetic game files/processes. A real engine's version/help execution was tested,
not a real modlist installation or game boot.

Sources inspected on 2026-10-13:

- [Jackify overview](https://github.com/Omni-guides/Jackify).
- [Jackify user guide](https://github.com/Omni-guides/Jackify/wiki/User-Guide).
- [Jackify engine adapter](https://github.com/Omni-guides/Jackify/blob/4134bdf729d750ec5078ba8d649edc2edf54a4b6/jackify/backend/services/engine_invoker.py).
- [Jackify engine](https://github.com/Omni-guides/dev-jackify-engine).
- [CLF3 output differences](https://github.com/SulfurNitride/CLF3).
- [Steam networking messages](https://partner.steamgames.com/doc/api/ISteamNetworkingMessages).


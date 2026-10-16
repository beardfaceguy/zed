# The Last Caretaker modding notes

Research notes for modding [The Last Caretaker](https://thelastcaretaker.com)
(Steam app 1783560). Written 2026-10-16.

Goals:

1. A **grappling hook gun**. It fires a hook a set distance, the hook latches
   onto a surface, and a powered winch pulls the character up.
2. A **crane** the player can build. It picks up cargo and lifts it to a
   higher level.

See [linux-setup.md](linux-setup.md) for setting up the workspace on Linux
(Proton) and how to split work with a Windows PC.

No grappling-hook or crane mod existed as of this writing. There is no
official mod kit or Steam Workshop. Everything below is community tooling.

Nexus Mods could not be reached from the research environment. Information
about Nexus pages comes from search-result snippets, not from reading the
pages directly.

## How modding this game works

The game's internal name is **Voyage**. It runs on Unreal Engine 5.8.1.
Earlier builds were 5.7, so updates do change the engine version.

There are three levels of mod:

| Level | What it is | Where it goes | Good for |
|---|---|---|---|
| **UE4SS Lua** | Scripts that read and change live game objects, add keybinds, and run code every tick | `Voyage\Binaries\Win64\ue4ss\Mods\<Name>\Scripts\main.lua` (plus an `enabled.txt`) | Quick prototypes and behavior tweaks. Cannot add new 3D models or items. |
| **Pak mods** | Cooked assets that override or add game content | `Voyage\Content\Paks\~mods`, or `Content\Paks\LogicMods` when using the Blueprint mod loader (DML) | Changes to existing assets, Blueprint logic |
| **Additive content (IoStore)** | New Blueprints, items, recipes, and models built in the UE 5.8 editor, cooked, and packed into `.pak`/`.ucas`/`.utoc` files | `Content\Paks` plus a small loose plugin descriptor under `Voyage\Mods\...` | A real craftable grappling gun or a buildable crane |

### UE4SS

- **The game needs its own build of UE4SS.** Stock UE4SS won't load. Use
  [UE4SS for TheLastCaretaker (Nexus mod 4)](https://www.nexusmods.com/thelastcaretaker/mods/4).
  At the time of writing it targets UE 5.8 and game version v0.8.5.x.
- Install it by extracting into `...\Voyage\Voyage\Binaries\Win64\`.
- The AES decryption key and the type-mapping file (`.usmap`) needed to open
  the game's files change with each update. Current copies are posted in the
  **#resources** channel of the community modding Discord (linked from the
  Nexus page).
- General modding technique: the
  [Dmgvol UE_Modding guide](https://github.com/Dmgvol/UE_Modding) and its
  [IoStore packing guide](https://github.com/Dmgvol/UE_Modding/blob/main/BasicModding/IoStorePacking.md).
- To reduce debug output, set the three variables under `[Debug]` in
  `ue4ss\UE4SS-settings.ini` to `0`.
- Mod load status and errors are written to
  `Voyage\Binaries\Win64\ue4ss\UE4SS.log`.
- Older mirror (bundles the AES key, mapping file, and guides):
  [VGTimes UE4SS download](https://vgtimes.com/games/the-last-caretaker/files/89333-ue4ss.html).

### Example mods on Nexus

Useful as references. Some are described by their authors as largely
AI-generated and not heavily tested, so treat them as examples, not vetted
code.

- [Admin Menu (F1 Cheat Menu)](https://www.nexusmods.com/thelastcaretaker/mods/230):
  plain Lua for UE4SS, no cooked assets, so engine updates don't break it.
  Layout is `enabled.txt` plus `Scripts/*.lua`.
- [Bow Thrusters](https://www.nexusmods.com/thelastcaretaker/mods/204): Lua,
  keybinds set in `Scripts/main.lua`.
- [Better Cables](https://www.nexusmods.com/thelastcaretaker/mods/129)
- [Automatic Pickup](https://www.nexusmods.com/thelastcaretaker/mods/128): the
  author moved the default to a pak version because the Lua version was more
  resource-intensive and failure-prone.
- [BuildAnywhere](https://www.nexusmods.com/thelastcaretaker/mods/125)
- [Hands-Free Mooring](https://www.nexusmods.com/thelastcaretaker/mods/172)
- [Turrets](https://www.nexusmods.com/thelastcaretaker/mods/231)
- [Infinite Enhancer Modules](https://www.nexusmods.com/thelastcaretaker/mods/108)

Other links:
[Steam "Mods" discussion](https://steamcommunity.com/app/1783560/discussions/0/728028631117517716/),
[Vortex extension](https://www.nexusmods.com/site/mods/1624),
[DarkHacks ModPack Creator](https://www.nexusmods.com/thelastcaretaker/mods/141).

### Asset tools

- [retoc](https://github.com/trumank/retoc): packs and unpacks IoStore
  containers (`.utoc`/`.ucas`). The upstream version may not handle UE 5.8;
  ihsoft maintains a 5.8-compatible fork (see below).
- [UnrealReZen](https://github.com/rm-NoobInCoding/UnrealReZen): packs IoStore
  containers.
- FModel: browses and extracts game assets (needs the AES key and `.usmap`).
- [UE-Modding-Tools databank](https://github.com/Buckminsterfullerene02/UE-Modding-Tools):
  a catalog of UE modding tools.

## The most useful find: ihsoft/TheLastCaretakerMods

[ihsoft/TheLastCaretakerMods](https://github.com/ihsoft/TheLastCaretakerMods)
is an open-source modding workspace for this game, designed to be driven by a
coding agent (it has `AGENTS.md` files and extensive docs). It is not a
collection of ready-to-install mods: you build each mod from source against
your installed game.

### Mods in the repo

- **Railgun**: a new, buildable, powered weapon. It has its own model,
  recipe, research unlock, an operator station you get into and control, a
  HUD, projectiles and ammo, packaged as a self-contained add-on. **This is
  the closest existing template for the crane, and for adding any new item.**
  Controls: mouse aims, left mouse fires, right mouse toggles the scope, `E`
  exits, `R` reloads from the operator's inventory.
- **MooringCable60m**: changes the game's built-in cable system so mooring
  cables can be deployed to 60 m. Its notes document the cable system (see
  below), which could serve as the rope and winch for both of our mods.
- **DonkLiftKeyboardControl**: keyboard control for the DonkLift-320
  forklift, kept in both a Lua/UE4SS version (`DonkLiftKeyboardControlUE4SS`)
  and an add-on version. Useful for comparing the two approaches.
- Others: BoatHUDTotalResources, ContainerLidLabel, ElectrifiedBoat,
  ScopeFix, StableGyro, VoyageAutoLoader.

### Tools in the repo

PowerShell scripts for:

- identifying the installed game build (`Get-VoyageBuildFingerprint.ps1`);
- resolving or generating the type mapping (`Get-VoyageMappings.ps1`,
  `New-VoyageMappings.ps1`, and a UE4SS Lua mappings dumper);
- extracting and inspecting assets (`Extract-VoyagePackage.ps1`,
  `Inspect-VoyageAsset.ps1`, `Get-VoyageAssetSummary.ps1`);
- building the UE 5.8 fork of retoc (`Build-RetocUe58Compatibility.ps1`);
- installing and restoring mods with hash checks
  (`Install-VoyageRelease.ps1`, `Restore-VoyageReleaseInstallation.ps1`).

Its accepted UE 5.8 toolchain uses ihsoft forks of retoc, jmap, UAssetAPI, and
UAssetGUI, plus upstream CUE4Parse. Railgun is built with the UE 5.8.2 editor.

### Caveats

- Heavily PowerShell-driven (Windows PowerShell 5.1) with hard-coded drive
  paths (`K:\Epic Games\UE_5.8`, `P:\...`).
- Pins exact game builds and refuses to build on a mismatch (some builders
  have an `-IgnorePinnedGameVersion` flag).
- Some notes are in Russian.

### Game internals documented there

- **Hand tools** follow a pattern: an item data asset (for example
  `/Game/Data/Assets/Equipment/Hands/DA_Equipment_ScanTool`), a Blueprint for
  the equipment (`/Game/Blueprints/Equipment/BP_Equipment_ScanTool`), and
  "ToolAbility" Blueprints (for example
  `/Game/Blueprints/Equipment/ToolAbilities/BP_ToolAbility_Scan`). A
  grappling gun would be a new hand tool built the same way.
- **Player classes**: `VoyageCharacter`, `VoyagePlayerController`.
- **Cables**: `/Game/Blueprints/Cables/BP_ModuleCable_Base` derives from the
  native class `ModulesConnectCable` and sets `MaxLengthBase = 2000` cm.
  Deployment checks both `MaxLength` and `MaxLengthBase`. `CableSnapLength`
  is separate. Connected cables have native payout (reeling) behavior and a
  tension force:
  `D = CableLengthCurrent - (1 + CableTightness) * NumSegments * CableWidth`;
  no force when `D <= 0`, otherwise force includes
  `(D * CableStretchForceMultiplier + CableStretchForceOffset) * CableStrength`,
  followed by velocity, mass, clamping, and timestep terms.
- **New items with recipes** need a native item definition (`VoyageItem`), a
  research unlock (`VoyageSkill`), and an asset-registry entry so the game's
  Asset Manager can find them. A recipe showing in the fabricator does not
  prove it will fabricate or drop correctly; test the spawned box's contents.
  Railgun ships a three-record plugin-local `AssetRegistry.bin` inside its
  pak plus a loose `.uplugin` descriptor.
- **Vehicles**: the forklift is `VoyageVehicleForkliftPawn` (native) with the
  Blueprint `BP_Forklift_Possesable_C`. Enhanced Input, the native pawn, the
  Blueprint's own `Acceleration`/`Steering` values, and the HUD are separate
  layers.
- **DML** (Blueprint mod loader) v0.6 discovers mods in
  `Voyage/Content/Paks/LogicMods`; install each mod's `.pak`, `.ucas`, and
  `.utoc` together there.

## Grappling hook plan

### Step 1: Lua prototype (days, not weeks)

A UE4SS script that:

1. On a keybind, traces a line from the camera out to a maximum distance (say
   40 m). If it hits a surface, that point becomes the anchor.
2. Every tick while the winch is reeling in, sets the character's movement to
   falling or flying and pushes them toward the anchor at the winch speed. It
   slows near the top and releases when they arrive or press the key again.
3. Optionally draws a debug line as the rope.

This uses no new assets, so it should survive game updates fairly well, and
it tells us whether the movement feels good before investing in the full
version.

### Step 2: a real item

A new hand tool in the UE 5.8 editor (a Blueprint child of a stock equipment
class), with:

- a model and a fabricator recipe (plus research unlock);
- a projectile hook that sticks where it hits;
- a cable component for the rope visual, or the game's own cable actor;
- the winch logic from step 1, possibly drawing power from the suit or a
  battery.

Cooked and packed the same way as Railgun.

## Crane plan

Harder, because it's a buildable structure rather than something you hold.
Railgun already solves the "buildable thing with an operator seat" part. The
crane needs:

- a base module with an operator station;
- a boom that rotates and extends;
- a hook on a cable, possibly the game's own cable with its length driven by
  a winch;
- grabbing cargo, either by attaching to the cargo's physics object or
  holding it with a physics constraint.

A cheaper Lua proof of concept: a keybind that picks the nearest physics
object and lifts it smoothly to a target height. Separately, check whether
the forklift's lift code can be reused.

**Build the grappling hook first.** It teaches the same toolchain on a
smaller problem.

## Next steps on the machine with the game

1. Install the game-specific UE4SS. Turn on its live object viewer and
   header dump so we can find the real class names for the character,
   cables, equipment, and modules on the installed build.
2. Get the current AES key and `.usmap` from the modding Discord. Install
   FModel to browse assets.
3. For step 2 only: install Unreal Engine 5.8, Visual Studio with C++,
   .NET 10, and clone ihsoft's repo for its tools.
4. Start with the Lua grappling-hook prototype.

Warnings:

- The game is still getting frequent updates. Expect mods to need
  re-validation after patches.
- Multiplayer sync has not been considered.

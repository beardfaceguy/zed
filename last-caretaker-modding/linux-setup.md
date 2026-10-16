# Modding The Last Caretaker on Linux

The game runs on Linux through Proton. The Linux machine works well for the
first stage, the Lua prototypes. The later stage, making real new items and
models, is much easier on a Windows PC. Plan to use both.

See [README.md](README.md) for the overall research and mod plans.

## Lua prototyping on Linux: works

UE4SS can be loaded under Proton.

1. **Find the game folder.** Usually
   `~/.steam/steam/steamapps/common/Voyage/` or
   `~/.local/share/Steam/steamapps/common/Voyage/`. Mods go in the same
   relative paths as on Windows (`Voyage/Binaries/Win64/ue4ss/...`).
2. **Install the game-specific UE4SS** from
   [Nexus mod 4](https://www.nexusmods.com/thelastcaretaker/mods/4) into
   `Voyage/Binaries/Win64/`, exactly as on Windows.
3. **Tell Proton to load UE4SS's DLL.** In Steam, open the game's
   Properties and set the launch option:

   ```
   WINEDLLOVERRIDES="dwmapi=n,b" %command%
   ```

   - The loader file is probably `dwmapi.dll`, but this is not verified for
     this game's UE4SS build. Check which DLL the zip actually puts next to
     the game's `.exe`, and use that name in the override.
   - Proton is stricter about where DLLs sit. If UE4SS doesn't start,
     [UE4SS issue #1189](https://github.com/UE4SS-RE/RE-UE4SS/issues/1189)
     describes moving `UE4SS.dll` into the same folder as `dwmapi.dll`.
   - If there is still trouble, try GE-Proton (a community Proton build);
     [GE-Proton 10-16](https://en.linuxadictos.com/GE-Proton-10-16-arrives-with-improvements-for-Stellar-Blade-Star-Citizen-and-updated-components.html)
     listed a UE4SS fix in its changelog.
4. **Run Claude Code natively on Linux in that folder.** Editing `main.lua`,
   reading `UE4SS.log`, and reading UE4SS's object and header dumps are all
   plain files.

## Asset tools on Linux: mostly fine

| Tool | Linux? |
|---|---|
| retoc, repak (pack/unpack game archives) | Yes, native Rust. The UE 5.8 fork may need building from source. |
| CUE4Parse (the library FModel is built on) | Yes, .NET is cross-platform. A small command-line extractor is feasible. |
| FModel (asset browser) | Windows-only GUI. Might run under Wine, but easier on the Windows PC. |
| ihsoft's workspace scripts | No. Windows PowerShell 5.1 with Windows paths and Windows-only process handling. They would need porting. |

## New items and models: use the Windows PC

The full grappling-gun item and the crane need the **Unreal Engine 5.8
editor** to create Blueprints and **cook** assets for a Windows game.

- Unreal runs on Linux, but cooking for Windows from Linux is poorly
  supported; shader compilation for Windows is the usual sticking point. Not
  confirmed to fail on 5.8, but not a workflow to rely on.
- Every proven pipeline for this game, including Railgun's, runs on Windows.
- Built mods are the same `.pak`/`.ucas`/`.utoc` files whichever OS runs the
  game. Copy them into the Linux install and test there.

## Suggested setup

- **Linux (daily):** the game, UE4SS, Claude Code, and a git repo for the
  mods. Prototype and test the grapple and crane logic in Lua here.
- **Windows PC (when needed):** UE 5.8 editor, Visual Studio, .NET, FModel,
  and a clone of ihsoft's repo. Run Claude Code there too, pointed at the
  same git repo, for build-and-cook steps.
- Share work through git: push from one machine, pull on the other. Copy
  built mod files to the Linux game folder over the network or with a sync
  tool.

## First steps on the Linux box

1. Find the game path and Proton prefix.
2. Install UE4SS and confirm it loads (`UE4SS.log` shows mods started).
3. Turn on the object dump to find the real character and cable class names.
4. Write the grapple prototype.

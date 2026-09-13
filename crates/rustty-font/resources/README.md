# Bundled fonts

These files are unchanged copies of Ghostty's pinned production font resources
from `build.zig.zon` and `src/build/SharedDeps.zig`:

- JetBrains Mono 2.304: `fonts/variable/JetBrainsMono[wght].ttf` and
  `fonts/variable/JetBrainsMono-Italic[wght].ttf`. See `JetBrainsMono-OFL.txt`.
- NerdFontsSymbolsOnly 3.4.0: `SymbolsNerdFont-Regular.ttf`.
  See `NerdFontsSymbols-LICENSE.txt`.

The font bytes are embedded so neither a Zig dependency cache nor network access
is needed at build or runtime. macOS supplies Apple Color Emoji and additional
fallback faces through CoreText; these system fonts are not redistributed.

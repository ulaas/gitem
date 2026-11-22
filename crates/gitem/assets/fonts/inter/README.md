# Inter

Inter Regular and SemiBold, from the [Inter project](https://github.com/rsms/inter), under the
SIL Open Font License 1.1 in `LICENSE.txt`.

These copies leave out Inter's private-use code points (U+E000 to U+F8FF). The interface icons
come from Phosphor, which uses the same range, and Inter comes first among the fonts, so its own
glyphs there would show in place of the icons. Nothing else is changed. They were made with
fontTools:

```sh
pyftsubset Inter-Regular.ttf --unicodes="U+0000-DFFF,U+F900-FFFF,U+10000-10FFFF" \
  --layout-features='*' --glyph-names --name-IDs='*' --name-languages='*' \
  --notdef-outline --hinting --output-file=Inter-Regular.ttf
```

and the same for `Inter-SemiBold.ttf`.

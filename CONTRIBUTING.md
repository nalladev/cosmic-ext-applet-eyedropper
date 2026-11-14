# Contributing

Contributions are welcome! For build and development commands, see the [README](README.md#development).

## Adding a translation

Thanks for helping translate Eyedropper! There are three places to check when adding a language. If you're unsure about any step, open a PR with what you have and ask for help.

1. **Text inside the app — `i18n/`**
   - Copy the [English file](i18n/en/cosmic_ext_applet_eyedropper.ftl) into a folder for your language, such as `i18n/fr/` for French. Keep the filename `cosmic_ext_applet_eyedropper.ftl`.
   - Translate the text after `=`, keeping the name before `=` unchanged. For example, `about = About` becomes `about = À propos`.
   - Keep placeholders such as `{ $num }`, `{ $hash }`, and `{ $date }` unchanged; the app fills these in.
   - You don't need to change any Rust code or register your language elsewhere.

2. **Store description — [`resources/app.metainfo.xml`](resources/app.metainfo.xml)**
   - Follow the existing translations to add your language's summary, description, feature list, screenshot captions, and keywords. Translate the app name if appropriate.
   - Use your language code in `xml:lang`, for example `xml:lang="fr"`. On feature lists, add it to both `<ul>` and each `<li>`, as in the existing examples.
   - Keep the English text and links. There's no need to change release notes.

3. **Desktop name and description — [`resources/app.desktop`](resources/app.desktop)**
   - Add a translated description, for example `Comment[fr]=...`.
   - If you translated the app name, also add `Name[fr]=...`.
   - Keep the existing English entries.

Before opening your PR, check that you haven't changed the names before `=` or the placeholders in the translation file. Mention anything left untranslated or anything you'd like help with.

Updating an existing translation? Only change the files that need updating.

### Optional checks and tips

You don't need to build the app or install extra tools to contribute a translation.

- If you can build the app, run `just build-debug` to include your translation. Try it with your desktop language set to the target language and look for missing text or text that doesn't fit.
- If you already have `appstreamcli`, you can check the store description file with `appstreamcli validate --no-net resources/app.metainfo.xml`.
- For regional language codes, the format differs slightly: for example, use `pt-BR` for the `i18n/` folder and `xml:lang`, but `pt_BR` inside the brackets in `app.desktop`.
- Missing app translations fall back to English.

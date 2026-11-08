# File-type icon sources

Every SVG in this folder is a real file-type icon from the **vscode-icons**
project (https://github.com/vscode-icons/vscode-icons), license **MIT**
(see the `LICENSE` file in that repository). They are vendored verbatim so the
console can show them without fetching anything at run time.

| File             | Upstream URL                                                                                                              |
| ---------------- | ------------------------------------------------------------------------------------------------------------------------- |
| `pdf.svg`        | https://raw.githubusercontent.com/vscode-icons/vscode-icons/master/icons/file_type_pdf.svg        |
| `powerpoint.svg` | https://raw.githubusercontent.com/vscode-icons/vscode-icons/master/icons/file_type_powerpoint.svg |
| `excel.svg`      | https://raw.githubusercontent.com/vscode-icons/vscode-icons/master/icons/file_type_excel.svg      |
| `word.svg`       | https://raw.githubusercontent.com/vscode-icons/vscode-icons/master/icons/file_type_word.svg       |
| `markdown.svg`   | https://raw.githubusercontent.com/vscode-icons/vscode-icons/master/icons/file_type_markdown.svg   |
| `image.svg`      | https://raw.githubusercontent.com/vscode-icons/vscode-icons/master/icons/file_type_image.svg      |
| `video.svg`      | https://raw.githubusercontent.com/vscode-icons/vscode-icons/master/icons/file_type_video.svg      |
| `audio.svg`      | https://raw.githubusercontent.com/vscode-icons/vscode-icons/master/icons/file_type_audio.svg      |
| `zip.svg`        | https://raw.githubusercontent.com/vscode-icons/vscode-icons/master/icons/file_type_zip.svg        |
| `json.svg`       | https://raw.githubusercontent.com/vscode-icons/vscode-icons/master/icons/file_type_json.svg       |
| `csv.svg`        | https://raw.githubusercontent.com/vscode-icons/vscode-icons/master/icons/file_type_csv.svg        |
| `text.svg`       | https://raw.githubusercontent.com/vscode-icons/vscode-icons/master/icons/file_type_text.svg       |

Downloaded 2026-11-08 from the `master` branch. To add another type, download
`icons/file_type_<name>.svg` from the same repo, add it to this table, and map
its extensions in `src/file_icons.rs`.

import {
  mdiLanguageTypescript,
  mdiLanguageJavascript,
  mdiVuejs,
  mdiLanguageHtml5,
  mdiLanguageCss3,
  mdiSass,
  mdiLanguagePython,
  mdiLanguageRust,
  mdiLanguageGo,
  mdiLanguageJava,
  mdiLanguageCpp,
  mdiLanguageC,
  mdiLanguageCsharp,
  mdiLanguageRuby,
  mdiLanguagePhp,
  mdiLanguageSwift,
  mdiLanguageKotlin,
  mdiLanguageLua,
  mdiLanguageHaskell,
  mdiLanguageMarkdown,
  mdiLanguageFortran,
  mdiLanguageR,
  mdiCodeJson,
  mdiXml,
  mdiSvg,
  mdiDocker,
  mdiGit,
  mdiNpm,
  mdiTailwind,
  mdiDatabase,
  mdiCog,
  mdiLock,
  mdiFileImage,
  mdiFileMusic,
  mdiFileVideo,
  mdiFileDocument,
  mdiFileCertificate,
  mdiFileCode,
  mdiFileOutline,
  mdiZipBox,
  mdiConsole,
  mdiText,
  mdiFileDelimited,
} from '@mdi/js'

interface FileIconDef {
  icon: string
  color: string
}

const extMap: Record<string, FileIconDef> = {
  // TypeScript
  ts:   { icon: mdiLanguageTypescript, color: '#3178c6' },
  tsx:  { icon: mdiLanguageTypescript, color: '#3178c6' },
  mts:  { icon: mdiLanguageTypescript, color: '#3178c6' },
  cts:  { icon: mdiLanguageTypescript, color: '#3178c6' },
  // JavaScript
  js:   { icon: mdiLanguageJavascript, color: '#f1e05a' },
  jsx:  { icon: mdiLanguageJavascript, color: '#f1e05a' },
  mjs:  { icon: mdiLanguageJavascript, color: '#f1e05a' },
  cjs:  { icon: mdiLanguageJavascript, color: '#f1e05a' },
  // Vue
  vue:  { icon: mdiVuejs, color: '#41b883' },
  // React
  // (tsx/jsx already covered above)
  // HTML
  html: { icon: mdiLanguageHtml5, color: '#e34c26' },
  htm:  { icon: mdiLanguageHtml5, color: '#e34c26' },
  // CSS
  css:  { icon: mdiLanguageCss3, color: '#563d7c' },
  scss: { icon: mdiSass, color: '#cc6699' },
  sass: { icon: mdiSass, color: '#cc6699' },
  less: { icon: mdiLanguageCss3, color: '#1d365d' },
  // Python
  py:   { icon: mdiLanguagePython, color: '#3572a5' },
  pyi:  { icon: mdiLanguagePython, color: '#3572a5' },
  pyw:  { icon: mdiLanguagePython, color: '#3572a5' },
  // Rust
  rs:   { icon: mdiLanguageRust, color: '#dea584' },
  // Go
  go:   { icon: mdiLanguageGo, color: '#00add8' },
  // Java
  java: { icon: mdiLanguageJava, color: '#b07219' },
  jar:  { icon: mdiLanguageJava, color: '#b07219' },
  // C/C++
  c:    { icon: mdiLanguageC, color: '#555555' },
  h:    { icon: mdiLanguageC, color: '#555555' },
  cpp:  { icon: mdiLanguageCpp, color: '#f34b7d' },
  cc:   { icon: mdiLanguageCpp, color: '#f34b7d' },
  cxx:  { icon: mdiLanguageCpp, color: '#f34b7d' },
  hpp:  { icon: mdiLanguageCpp, color: '#f34b7d' },
  // C#
  cs:   { icon: mdiLanguageCsharp, color: '#178600' },
  // Ruby
  rb:   { icon: mdiLanguageRuby, color: '#701516' },
  // PHP
  php:  { icon: mdiLanguagePhp, color: '#4f5d95' },
  // Swift
  swift: { icon: mdiLanguageSwift, color: '#f05138' },
  // Kotlin
  kt:   { icon: mdiLanguageKotlin, color: '#a97bff' },
  kts:  { icon: mdiLanguageKotlin, color: '#a97bff' },
  // Lua
  lua:  { icon: mdiLanguageLua, color: '#000080' },
  // Haskell
  hs:   { icon: mdiLanguageHaskell, color: '#5e5086' },
  // Fortran
  f90:  { icon: mdiLanguageFortran, color: '#4d41b1' },
  f95:  { icon: mdiLanguageFortran, color: '#4d41b1' },
  // R
  r:    { icon: mdiLanguageR, color: '#198ce7' },
  // Markdown
  md:   { icon: mdiLanguageMarkdown, color: '#083fa1' },
  mdx:  { icon: mdiLanguageMarkdown, color: '#083fa1' },
  // JSON
  json: { icon: mdiCodeJson, color: '#a1a100' },
  jsonc: { icon: mdiCodeJson, color: '#a1a100' },
  json5: { icon: mdiCodeJson, color: '#a1a100' },
  // XML
  xml:  { icon: mdiXml, color: '#e44d26' },
  xsl:  { icon: mdiXml, color: '#e44d26' },
  xslt: { icon: mdiXml, color: '#e44d26' },
  // SVG
  svg:  { icon: mdiSvg, color: '#ffb13b' },
  // YAML / TOML
  yml:  { icon: mdiFileCode, color: '#cb171e' },
  yaml: { icon: mdiFileCode, color: '#cb171e' },
  toml: { icon: mdiFileCode, color: '#9c4121' },
  // Config
  ini:  { icon: mdiCog, color: '#8c8c8c' },
  conf: { icon: mdiCog, color: '#8c8c8c' },
  cfg:  { icon: mdiCog, color: '#8c8c8c' },
  env:  { icon: mdiCog, color: '#8c8c8c' },
  // Shell
  sh:   { icon: mdiConsole, color: '#89e051' },
  bash: { icon: mdiConsole, color: '#89e051' },
  zsh:  { icon: mdiConsole, color: '#89e051' },
  fish: { icon: mdiConsole, color: '#89e051' },
  bat:  { icon: mdiConsole, color: '#c1f12e' },
  cmd:  { icon: mdiConsole, color: '#c1f12e' },
  ps1:  { icon: mdiConsole, color: '#012456' },
  // Data
  csv:  { icon: mdiFileDelimited, color: '#237346' },
  tsv:  { icon: mdiFileDelimited, color: '#237346' },
  sql:  { icon: mdiDatabase, color: '#e38c00' },
  db:   { icon: mdiDatabase, color: '#e38c00' },
  sqlite: { icon: mdiDatabase, color: '#e38c00' },
  // Images
  png:  { icon: mdiFileImage, color: '#a074c4' },
  jpg:  { icon: mdiFileImage, color: '#a074c4' },
  jpeg: { icon: mdiFileImage, color: '#a074c4' },
  gif:  { icon: mdiFileImage, color: '#a074c4' },
  webp: { icon: mdiFileImage, color: '#a074c4' },
  ico:  { icon: mdiFileImage, color: '#a074c4' },
  bmp:  { icon: mdiFileImage, color: '#a074c4' },
  // Audio
  mp3:  { icon: mdiFileMusic, color: '#e44d26' },
  wav:  { icon: mdiFileMusic, color: '#e44d26' },
  ogg:  { icon: mdiFileMusic, color: '#e44d26' },
  flac: { icon: mdiFileMusic, color: '#e44d26' },
  // Video
  mp4:  { icon: mdiFileVideo, color: '#e44d26' },
  mkv:  { icon: mdiFileVideo, color: '#e44d26' },
  avi:  { icon: mdiFileVideo, color: '#e44d26' },
  webm: { icon: mdiFileVideo, color: '#e44d26' },
  // Archives
  zip:  { icon: mdiZipBox, color: '#e38c00' },
  gz:   { icon: mdiZipBox, color: '#e38c00' },
  tar:  { icon: mdiZipBox, color: '#e38c00' },
  rar:  { icon: mdiZipBox, color: '#e38c00' },
  '7z': { icon: mdiZipBox, color: '#e38c00' },
  // Docs
  pdf:  { icon: mdiFileDocument, color: '#e44d26' },
  doc:  { icon: mdiFileDocument, color: '#2b579a' },
  docx: { icon: mdiFileDocument, color: '#2b579a' },
  txt:  { icon: mdiText, color: '#8c8c8c' },
  // Certs / keys
  pem:  { icon: mdiFileCertificate, color: '#e38c00' },
  crt:  { icon: mdiFileCertificate, color: '#e38c00' },
  key:  { icon: mdiLock, color: '#e38c00' },
  // Lock files
  lock: { icon: mdiLock, color: '#8c8c8c' },
}

const nameMap: Record<string, FileIconDef> = {
  'Dockerfile':     { icon: mdiDocker, color: '#2496ed' },
  'docker-compose.yml': { icon: mdiDocker, color: '#2496ed' },
  'docker-compose.yaml': { icon: mdiDocker, color: '#2496ed' },
  '.gitignore':     { icon: mdiGit, color: '#f05032' },
  '.gitmodules':    { icon: mdiGit, color: '#f05032' },
  '.gitattributes': { icon: mdiGit, color: '#f05032' },
  'package.json':   { icon: mdiNpm, color: '#cb3837' },
  'package-lock.json': { icon: mdiNpm, color: '#cb3837' },
  '.npmrc':         { icon: mdiNpm, color: '#cb3837' },
  'tsconfig.json':  { icon: mdiLanguageTypescript, color: '#3178c6' },
  'tsconfig.app.json': { icon: mdiLanguageTypescript, color: '#3178c6' },
  'tsconfig.node.json': { icon: mdiLanguageTypescript, color: '#3178c6' },
  'tailwind.config.js': { icon: mdiTailwind, color: '#06b6d4' },
  'tailwind.config.ts': { icon: mdiTailwind, color: '#06b6d4' },
  'Cargo.toml':     { icon: mdiLanguageRust, color: '#dea584' },
  'Cargo.lock':     { icon: mdiLanguageRust, color: '#dea584' },
  'Makefile':       { icon: mdiConsole, color: '#427819' },
  'CMakeLists.txt': { icon: mdiConsole, color: '#427819' },
  'LICENSE':        { icon: mdiFileDocument, color: '#e38c00' },
  'CLAUDE.md':      { icon: mdiFileDocument, color: '#d97706' },
}

const defaultIcon: FileIconDef = { icon: mdiFileOutline, color: '#8c8c8c' }

export function getFileIcon(fileName: string): FileIconDef {
  // Check exact name first
  const byName = nameMap[fileName]
  if (byName) return byName

  // Check extension
  const dotIdx = fileName.lastIndexOf('.')
  if (dotIdx >= 0) {
    const ext = fileName.slice(dotIdx + 1).toLowerCase()
    const byExt = extMap[ext]
    if (byExt) return byExt
  }

  return defaultIcon
}

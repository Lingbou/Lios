export type FileCategory =
  | "code"
  | "image"
  | "doc"
  | "sheet"
  | "archive"
  | "media"
  | "default";

const codeExtensions = new Set([
  "js", "jsx", "ts", "tsx", "py", "rs", "go", "java", "c", "cpp", "h", "hpp",
  "cs", "html", "css", "scss", "sass", "less", "json", "yaml", "yml", "toml",
  "md", "sh", "bash", "zsh", "sql", "xml", "lua", "php", "rb", "swift", "kt"
]);

const imageExtensions = new Set([
  "png", "jpg", "jpeg", "gif", "svg", "webp", "bmp", "ico", "tiff", "tif", "psd", "raw", "heic"
]);

const docExtensions = new Set([
  "pdf", "doc", "docx", "txt", "rtf", "odt", "epub", "pages", "tex"
]);

const sheetExtensions = new Set([
  "xls", "xlsx", "csv", "tsv", "parquet", "arrow", "feather", "ods", "numbers"
]);

const archiveExtensions = new Set([
  "zip", "tar", "gz", "7z", "rar", "bz2", "xz", "tgz", "zst", "iso", "dmg"
]);

const mediaExtensions = new Set([
  "mp4", "mkv", "avi", "mov", "wmv", "flv", "webm", "m4v", "mp3", "wav", "flac", "aac", "ogg", "m4a", "wma"
]);

export function getFileExtension(filename: string): string {
  const dotIndex = filename.lastIndexOf(".");
  if (dotIndex <= 0 || dotIndex === filename.length - 1) return "";
  return filename.slice(dotIndex + 1).toLowerCase();
}

export function getFileCategory(filename: string): FileCategory {
  const ext = getFileExtension(filename);
  if (!ext) return "default";
  if (codeExtensions.has(ext)) return "code";
  if (imageExtensions.has(ext)) return "image";
  if (docExtensions.has(ext)) return "doc";
  if (sheetExtensions.has(ext)) return "sheet";
  if (archiveExtensions.has(ext)) return "archive";
  if (mediaExtensions.has(ext)) return "media";
  return "default";
}

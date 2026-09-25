/**
 * Maps arbitrary user-friendly space names (including Chinese) to
 * ModelScope-compliant ASCII repository identifiers ([a-z][a-z0-9_-]{0,31}).
 */

const PINYIN_DICT: Record<string, string> = {
  测: "ce", 试: "shi", 上: "shang", 传: "chuan", 下: "xia", 载: "zai",
  工: "gong", 作: "zuo", 空: "kong", 间: "jian", 文: "wen", 档: "dang",
  备: "bei", 份: "fen", 相: "xiang", 册: "ce", 照: "zhao", 片: "pian",
  项: "xiang", 目: "mu", 学: "xue", 习: "xi", 笔: "bi", 记: "ji",
  资: "zi", 料: "liao", 代: "dai", 码: "ma", 视: "shi", 频: "pin",
  音: "yin", 乐: "yue", 归: "gui", 个: "ge", 人: "ren", 家: "jia",
  私: "si", 公: "gong", 共: "gong", 族: "zu", 常: "chang", 用: "yong",
  源: "yuan", 仓: "cang", 库: "ku", 电: "dian", 影: "ying", 我: "wo",
  的: "de", 新: "xin", 建: "jian", 临: "lin", 时: "shi", 日: "ri",
  生: "sheng", 活: "huo", 旅: "lv", 游: "you", 戏: "xi", 图: "tu",
  云: "yun", 盘: "pan", 存: "cun", 储: "chu", 密: "mi", 安: "an",
  全: "quan", 书: "shu", 籍: "ji", 漫: "man", 画: "hua", 动: "dong",
  画片: "huapian", 录: "lu", 像: "xiang", 音频: "yinpin", 大: "da",
  小: "xiao", 中: "zhong", 高: "gao", 低: "di", 快: "kuai", 慢: "man",
  好: "hao", 多: "duo", 少: "shao", 零: "ling", 一: "yi", 二: "er",
  三: "san", 四: "si", 五: "wu", 六: "liu", 七: "qi", 八: "ba",
  九: "jiu", 十: "shi", 百: "bai", 千: "qian", 万: "wan", 年: "nian",
  月: "yue", 天: "tian", 设: "she", 置: "zhi", 夹: "jia", 卷: "juan",
  软: "ruan", 件: "jian", 系: "xi", 统: "tong", 数: "shu", 据: "ju",
  集: "ji", 表: "biao", 单: "dan", 记事: "jishi", 备忘: "beiwang",
  账: "zhang", 本: "ben", 财务: "caiwu", 发: "fa", 票: "piao", 合: "he",
  同: "tong", 简: "jian", 历: "li", 报: "bao", 告: "gao", 论: "lun"
};

export function nameToSafeSlug(name: string): string {
  const trimmed = name.trim();
  if (/^[a-z][a-z0-9_-]{0,31}$/.test(trimmed)) {
    return trimmed;
  }

  const parts: string[] = [];
  for (let i = 0; i < trimmed.length; i++) {
    const ch = trimmed[i];
    if (/[a-zA-Z0-9]/.test(ch)) {
      parts.push(ch.toLowerCase());
    } else if (ch === "-" || ch === "_") {
      if (parts.length > 0 && parts[parts.length - 1] !== "_") {
        parts.push(ch);
      }
    } else if (/\s|[.,\/#!$%\^&\*;:{}=\-`~()\[\]<>?]/.test(ch)) {
      if (parts.length > 0 && parts[parts.length - 1] !== "_") {
        parts.push("_");
      }
    } else if (PINYIN_DICT[ch]) {
      if (parts.length > 0 && parts[parts.length - 1] !== "_") {
        parts.push("_");
      }
      parts.push(PINYIN_DICT[ch]);
    } else {
      const code = ch.charCodeAt(0);
      if (parts.length > 0 && parts[parts.length - 1] !== "_") {
        parts.push("_");
      }
      parts.push(code.toString(36));
    }
  }

  let slug = parts.join("").replace(/_+/g, "_").replace(/^[^a-z]+/, "").replace(/_+$/, "");
  if (!slug || !/^[a-z]/.test(slug)) {
    let hash = 0;
    for (let i = 0; i < trimmed.length; i++) {
      hash = ((hash << 5) - hash) + trimmed.charCodeAt(i);
      hash |= 0;
    }
    slug = `space_${Math.abs(hash).toString(36)}`;
  }

  return slug.slice(0, 32);
}

/**
 * Normalizes a ModelScope repository name into a valid Lios local space alias
 * matching ^[a-z][a-z0-9_-]{0,31}$.
 *
 * If `existingAliases` is provided, ensures uniqueness by appending a suffix.
 */
export function sanitizeSpaceAlias(repoName: string, existingAliases?: Set<string>): string {
  const trimmed = repoName.trim().toLowerCase();
  let cleaned = trimmed
    .replace(/[^a-z0-9_-]/g, "_")
    .replace(/_+/g, "_")
    .replace(/^_+|_+$/g, "");

  if (!/^[a-z]/.test(cleaned)) {
    cleaned = `s_${cleaned}`.replace(/_+/g, "_");
  }

  let base = cleaned.slice(0, 32).replace(/[-_]+$/, "");
  if (!/^[a-z]/.test(base)) {
    base = "space";
  }

  if (!existingAliases || !existingAliases.has(base)) {
    return base;
  }

  for (let i = 2; i < 1000; i++) {
    const suffix = `_${i}`;
    const candidate = `${base.slice(0, 32 - suffix.length)}${suffix}`;
    if (!existingAliases.has(candidate)) {
      return candidate;
    }
  }

  return base;
}

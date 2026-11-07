//! Experimental interface translations for navigation, toolbars, and context
//! menus. Anything not listed stays in English, which is also the fallback.
use std::sync::atomic::{AtomicU8, Ordering};

static LANG: AtomicU8 = AtomicU8::new(0);

pub const LANGUAGES: [(&str, &str); 4] = [("en", "English"), ("es", "Español"), ("tr", "Türkçe"), ("zh", "简体中文")];

pub fn set_language(code: &str) {
    let i = LANGUAGES.iter().position(|l| l.0 == code).unwrap_or(0);
    LANG.store(i as u8, Ordering::Relaxed);
}

/// (English, Spanish, Turkish, Simplified Chinese)
const TABLE: &[(&str, &str, &str, &str)] = &[
    ("Home", "Inicio", "Ana sayfa", "主页"),
    ("Graph", "Gráfico", "Grafik", "提交图"),
    ("Changes", "Cambios", "Değişiklikler", "更改"),
    ("Launchpad", "Launchpad", "Launchpad", "启动台"),
    ("Search", "Buscar", "Ara", "搜索"),
    ("Worktrees", "Worktrees", "Çalışma ağaçları", "工作树"),
    ("Contributors", "Colaboradores", "Katkıda bulunanlar", "贡献者"),
    ("Visual", "Visual", "Görsel", "可视化"),
    ("Treemap", "Mapa de árbol", "Ağaç haritası", "树状图"),
    ("Health", "Salud", "Sağlık", "健康"),
    ("Agents", "Agentes", "Ajanlar", "代理"),
    ("Review", "Revisión", "İnceleme", "审查"),
    ("Reflog", "Reflog", "Reflog", "引用日志"),
    ("Rebase", "Rebase", "Rebase", "变基"),
    ("Bisect", "Bisect", "Bisect", "二分查找"),
    ("Patches", "Parches", "Yamalar", "补丁"),
    ("Composer", "Compositor", "Oluşturucu", "编排"),
    ("Tools", "Herramientas", "Araçlar", "工具"),
    ("Config", "Configuración", "Yapılandırma", "配置"),
    ("Credentials", "Credenciales", "Kimlik bilgileri", "凭据"),
    ("Console", "Consola", "Konsol", "控制台"),
    ("Settings", "Ajustes", "Ayarlar", "设置"),
    ("Stash", "Guardar cambios", "Sakla", "贮藏"),
    ("Refresh", "Actualizar", "Yenile", "刷新"),
    ("Details", "Detalles", "Ayrıntılar", "详情"),
    ("Sidebar", "Barra lateral", "Kenar çubuğu", "侧边栏"),
    ("Show details", "Mostrar detalles", "Ayrıntıları göster", "显示详情"),
    ("Open changes", "Abrir cambios", "Değişiklikleri aç", "打开更改"),
    ("Compare with working tree", "Comparar con el árbol de trabajo", "Çalışma ağacıyla karşılaştır", "与工作树比较"),
    ("Compare with HEAD", "Comparar con HEAD", "HEAD ile karşılaştır", "与 HEAD 比较"),
    ("Browse files at this commit", "Ver archivos en este commit", "Bu committeki dosyalara göz at", "浏览此提交的文件"),
    ("Create branch here…", "Crear rama aquí…", "Burada dal oluştur…", "在此创建分支…"),
    ("Create tag here…", "Crear etiqueta aquí…", "Burada etiket oluştur…", "在此创建标签…"),
    ("Create worktree here…", "Crear worktree aquí…", "Burada çalışma ağacı oluştur…", "在此创建工作树…"),
    ("Undo commit", "Deshacer commit", "Commiti geri al", "撤销提交"),
    ("Reword…", "Cambiar mensaje…", "Mesajı değiştir…", "修改说明…"),
    ("Squash into previous…", "Combinar con el anterior…", "Öncekiyle birleştir…", "压缩到上一个…"),
    ("Drop…", "Descartar…", "Kaldır…", "丢弃…"),
    ("Cherry-pick…", "Cherry-pick…", "Cherry-pick…", "拣选…"),
    ("Revert…", "Revertir…", "Geri döndür…", "还原…"),
    ("Copy SHA", "Copiar SHA", "SHA'yı kopyala", "复制 SHA"),
    ("Copy message", "Copiar mensaje", "Mesajı kopyala", "复制说明"),
    ("Copy as patch", "Copiar como parche", "Yama olarak kopyala", "复制为补丁"),
    ("Open on remote", "Abrir en el remoto", "Uzak depoda aç", "在远程打开"),
    ("Merge into current…", "Fusionar en la actual…", "Geçerli dala birleştir…", "合并到当前分支…"),
    ("Rename…", "Renombrar…", "Yeniden adlandır…", "重命名…"),
    ("Delete…", "Eliminar…", "Sil…", "删除…"),
    ("Push…", "Enviar…", "Gönder…", "推送…"),
    ("Copy name", "Copiar nombre", "Adı kopyala", "复制名称"),
    ("Copy path", "Copiar ruta", "Yolu kopyala", "复制路径"),
    ("Open terminal here", "Abrir terminal aquí", "Burada terminal aç", "在此打开终端"),
    ("File history", "Historial del archivo", "Dosya geçmişi", "文件历史"),
    ("Open with blame", "Abrir con blame", "Blame ile aç", "以追溯方式打开"),
    ("Apply", "Aplicar", "Uygula", "应用"),
    ("Pop", "Aplicar y quitar", "Uygula ve kaldır", "弹出"),
];

/// The current language's text for an English label.
pub fn t(english: &str) -> &str {
    let lang = LANG.load(Ordering::Relaxed);
    if lang == 0 {
        return english;
    }
    match TABLE.iter().find(|r| r.0 == english) {
        Some(r) => match lang {
            1 => r.1,
            2 => r.2,
            _ => r.3,
        },
        None => english,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_has_no_blanks_or_duplicates() {
        let mut seen = std::collections::HashSet::new();
        for r in TABLE {
            assert!(seen.insert(r.0), "duplicate {}", r.0);
            assert!(!r.1.is_empty() && !r.2.is_empty() && !r.3.is_empty(), "{}", r.0);
        }
    }

    #[test]
    fn translates_and_falls_back() {
        set_language("es");
        assert_eq!(t("Refresh"), "Actualizar");
        assert_eq!(t("Not in the table"), "Not in the table");
        set_language("en");
        assert_eq!(t("Refresh"), "Refresh");
    }
}

use std::cell::RefCell;
use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};

use anyhow::{anyhow, Result};
use windows::core::{BSTR, Interface};
use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED,
};
use windows::Win32::UI::Accessibility::{
    CUIAutomation, IUIAutomation, IUIAutomation2, IUIAutomationCacheRequest, IUIAutomationElement,
    IUIAutomationExpandCollapsePattern, IUIAutomationInvokePattern,
    IUIAutomationLegacyIAccessiblePattern, IUIAutomationSelectionItemPattern,
    IUIAutomationTogglePattern, IUIAutomationTextPattern, IUIAutomationValuePattern,
    TreeScope_Subtree, UIA_AutomationIdPropertyId, UIA_BoundingRectanglePropertyId,
    UIA_ClassNamePropertyId, UIA_ControlTypePropertyId, UIA_ExpandCollapsePatternId,
    UIA_InvokePatternId, UIA_IsEnabledPropertyId, UIA_IsOffscreenPropertyId,
    UIA_LegacyIAccessiblePatternId, UIA_NamePropertyId, UIA_SelectionItemPatternId,
    UIA_TextPatternId, UIA_TogglePatternId, UIA_ValuePatternId,
};

use super::{sleep_ms, to_hwnd};

thread_local! {
    static AUTOMATION: RefCell<Option<IUIAutomation>> = const { RefCell::new(None) };
    // 元素句柄按 id 缓存，模型可以跨多次调用引用同一个控件，不必每次重新观察
    static CACHE: RefCell<HashMap<String, (i64, IUIAutomationElement, i32)>> = RefCell::new(HashMap::new());
}

fn automation() -> Result<IUIAutomation> {
    AUTOMATION.with(|slot| {
        if let Some(existing) = slot.borrow().as_ref() {
            return Ok(existing.clone());
        }
        unsafe {
            // 多线程套间：UIA 客户端在 MTA 下工作正常，也不会阻塞其它线程
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            let automation: IUIAutomation =
                CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER)?;
            // 卡死的窗口不能让整个服务器跟着卡住：连接 800ms、单次事务 2s 就放弃
            if let Ok(automation2) = automation.cast::<IUIAutomation2>() {
                let _ = automation2.SetConnectionTimeout(800);
                let _ = automation2.SetTransactionTimeout(2000);
            }
            *slot.borrow_mut() = Some(automation.clone());
            Ok(automation)
        }
    })
}

#[derive(Debug, Clone)]
pub struct Node {
    pub id: String,
    pub depth: u32,
    pub name: String,
    pub control_type: &'static str,
    pub automation_id: String,
    pub rect: RECT,
    pub enabled: bool,
}

fn control_type_name(id: i32) -> &'static str {
    match id {
        50000 => "Button",
        50001 => "Calendar",
        50002 => "CheckBox",
        50003 => "ComboBox",
        50004 => "Edit",
        50005 => "Hyperlink",
        50006 => "Image",
        50007 => "ListItem",
        50008 => "List",
        50009 => "Menu",
        50010 => "MenuBar",
        50011 => "MenuItem",
        50012 => "ProgressBar",
        50013 => "RadioButton",
        50014 => "ScrollBar",
        50015 => "Slider",
        50016 => "Spinner",
        50017 => "StatusBar",
        50018 => "Tab",
        50019 => "TabItem",
        50020 => "Text",
        50021 => "ToolBar",
        50022 => "ToolTip",
        50023 => "Tree",
        50024 => "TreeItem",
        50025 => "Custom",
        50026 => "Group",
        50027 => "Thumb",
        50028 => "DataGrid",
        50029 => "DataItem",
        50030 => "Document",
        50031 => "SplitButton",
        50032 => "Window",
        50033 => "Pane",
        50034 => "Header",
        50035 => "HeaderItem",
        50036 => "Table",
        50037 => "TitleBar",
        50038 => "Separator",
        50039 => "SemanticZoom",
        50040 => "AppBar",
        _ => "Unknown",
    }
}

/// 无名容器只在这些类型上有意义；其余无名节点不输出，省掉大量噪声 token。
fn worthwhile_without_name(control_type: i32) -> bool {
    matches!(
        control_type,
        50003 | 50004 | 50008 | 50009 | 50010 | 50018 | 50021 | 50023 | 50030 | 50036
    )
}

pub fn element_from_hwnd(hwnd: i64) -> Result<IUIAutomationElement> {
    let automation = automation()?;
    let handle: HWND = to_hwnd(hwnd)?;
    unsafe { Ok(automation.ElementFromHandle(handle)?) }
}

/// 该窗口是否暴露了 UI Automation 树。用于 capabilities 判断。
pub fn probe(hwnd: i64) -> bool {
    element_from_hwnd(hwnd)
        .and_then(|element| unsafe { Ok(element.CurrentName()) })
        .is_ok()
}

/// 诊断用：分阶段打印 UIA 调用耗时，定位到底卡在哪一步。
pub fn probe_report(hwnd: i64) {
    use std::io::Write;
    let mut out = std::io::stdout();
    let t = std::time::Instant::now();
    macro_rules! stage {
        ($name:expr, $value:expr) => {{
            match $value {
                Ok(value) => {
                    let _ = writeln!(out, "[{:>5}ms] {} ok", t.elapsed().as_millis(), $name);
                    let _ = out.flush();
                    Some(value)
                }
                Err(error) => {
                    let _ = writeln!(out, "[{:>5}ms] {} 失败: {error}", t.elapsed().as_millis(), $name);
                    let _ = out.flush();
                    None
                }
            }
        }};
    }

    let Some(automation) = stage!("automation", automation()) else { return };
    let Some(root) = stage!("ElementFromHandle", element_from_hwnd(hwnd)) else { return };
    let Some(walker) = stage!("ControlViewWalker", unsafe { automation.ControlViewWalker() }) else {
        return;
    };
    let Some(request) = stage!("CreateCacheRequest", unsafe { automation.CreateCacheRequest() }) else {
        return;
    };
    unsafe {
        for property in [
            UIA_NamePropertyId,
            UIA_ControlTypePropertyId,
            UIA_AutomationIdPropertyId,
            UIA_ClassNamePropertyId,
            UIA_BoundingRectanglePropertyId,
            UIA_IsEnabledPropertyId,
            UIA_IsOffscreenPropertyId,
        ] {
            if let Err(error) = request.AddProperty(property) {
                let _ = writeln!(out, "AddProperty 失败: {error}");
            }
        }
    }
    let _ = stage!("SetTreeScope", unsafe { request.SetTreeScope(TreeScope_Subtree) });
    let Some(root) = stage!("BuildUpdatedCache", unsafe { root.BuildUpdatedCache(&request) }) else {
        return;
    };

    let mut nodes = Vec::new();
    let mut cache = Vec::new();
    let mut state = WalkState {
        out: &mut nodes,
        cache: &mut cache,
        used: HashMap::new(),
        emitted: 0,
        visited: 0,
        max_visited: 1200,
        budget: 200,
        deadline: std::time::Instant::now() + std::time::Duration::from_millis(4000),
    };
    let started = std::time::Instant::now();
    unsafe { walk(&walker, &root, &request, hwnd, 0, 12, &mut state) };
    let visited = state.visited;
    drop(state);
    let _ = writeln!(
        out,
        "walk 结束: 输出 {} 个控件，访问 {} 个节点，耗时 {}ms",
        nodes.len(),
        visited,
        started.elapsed().as_millis()
    );
    for line in render(&nodes).lines().take(30) {
        let _ = writeln!(out, "{line}");
    }

    let popups = super::window::related_popups(hwnd);
    let _ = writeln!(out, "related_popups: {}", popups.len());
    for popup in &popups {
        let _ = writeln!(
            out,
            "  hwnd={} class={} title={:?} rect={}x{} visible={}",
            popup.hwnd, popup.class, popup.title, popup.rect.width, popup.rect.height, popup.visible
        );
        match observe_with_walker(popup.hwnd, 8, 60, true) {
            Ok(nodes) => {
                let _ = writeln!(out, "  → {} 个控件", nodes.len());
                for line in render(&nodes).lines().take(12) {
                    let _ = writeln!(out, "    {line}");
                }
            }
            Err(error) => {
                let _ = writeln!(out, "  → 读取失败: {error}");
            }
        }
    }
}

struct WalkState<'a> {
    out: &'a mut Vec<Node>,
    cache: &'a mut Vec<(String, IUIAutomationElement, i32)>,
    used: HashMap<String, u32>,
    emitted: usize,
    visited: usize,
    max_visited: usize,
    budget: usize,
    deadline: std::time::Instant,
}

impl WalkState<'_> {
    fn exhausted(&self) -> bool {
        self.emitted >= self.budget
            || self.visited >= self.max_visited
            || std::time::Instant::now() >= self.deadline
    }
}

unsafe fn walk(
    walker: &windows::Win32::UI::Accessibility::IUIAutomationTreeWalker,
    element: &IUIAutomationElement,
    request: &IUIAutomationCacheRequest,
    hwnd: i64,
    depth: u32,
    max_depth: u32,
    state: &mut WalkState<'_>,
) {
    if depth > max_depth || state.exhausted() {
        return;
    }
    state.visited += 1;

    let offscreen = element
        .CachedIsOffscreen()
        .map(|v| v.as_bool())
        .unwrap_or(false);
    if offscreen {
        // 不可见子树整棵跳过，既省 token 也避免模型点到看不见的东西
        return;
    }
    let name = element
        .CachedName()
        .map(|v| v.to_string())
        .unwrap_or_default();
    let control_type = element.CachedControlType().map(|v| v.0).unwrap_or(0);
    let automation_id = element
        .CachedAutomationId()
        .map(|v| v.to_string())
        .unwrap_or_default();
    let class = element
        .CachedClassName()
        .map(|v| v.to_string())
        .unwrap_or_default();
    let rect = element.CachedBoundingRectangle().unwrap_or_default();
    let enabled = element
        .CachedIsEnabled()
        .map(|v| v.as_bool())
        .unwrap_or(true);

    let emit = !name.trim().is_empty() || worthwhile_without_name(control_type);
    if emit {
        let mut hasher = DefaultHasher::new();
        hwnd.hash(&mut hasher);
        automation_id.hash(&mut hasher);
        name.hash(&mut hasher);
        control_type.hash(&mut hasher);
        class.hash(&mut hasher);
        let base = format!("e{:04x}", (hasher.finish() & 0xffff) as u16);
        let count = state.used.entry(base.clone()).or_insert(0);
        *count += 1;
        let id = if *count == 1 {
            base
        } else {
            format!("{base}-{count}")
        };
        let display_name = if name.chars().count() > 60 {
            format!("{}…", name.chars().take(60).collect::<String>())
        } else {
            name.clone()
        };
        state.out.push(Node {
            id: id.clone(),
            depth,
            name: display_name,
            control_type: control_type_name(control_type),
            automation_id,
            rect,
            enabled,
        });
        state.cache.push((id, element.clone(), control_type));
        state.emitted += 1;
    }

    let mut child = walker.GetFirstChildElementBuildCache(element, request).ok();
    while let Some(current) = child {
        walk(walker, &current, request, hwnd, depth + 1, max_depth, state);
        if state.exhausted() {
            break;
        }
        child = walker
            .GetNextSiblingElementBuildCache(&current, request)
            .ok();
    }
}

/// 遍历单个窗口的控件树，返回压缩后的节点列表，并把控件句柄缓存起来。
///
/// 三条护栏防止被卡死的窗口拖住：缓存请求（一次 IPC 取全部属性）、
/// 访问节点上限、2.5 秒墙钟上限。
pub fn observe_window(hwnd: i64, max_depth: u32, max_elements: usize) -> Result<Vec<Node>> {
    observe_with_walker(hwnd, max_depth, max_elements, false)
}

/// raw=true 用 RawViewWalker：WinUI 的弹出菜单只出现在原始视图里，控件视图会过滤掉。
fn observe_with_walker(
    hwnd: i64,
    max_depth: u32,
    max_elements: usize,
    raw: bool,
) -> Result<Vec<Node>> {
    let automation = automation()?;
    let root = element_from_hwnd(hwnd)?;
    let walker = unsafe {
        if raw {
            automation.RawViewWalker()?
        } else {
            automation.ControlViewWalker()?
        }
    };
    let request = unsafe { automation.CreateCacheRequest()? };
    let root = unsafe {
        for property in [
            UIA_NamePropertyId,
            UIA_ControlTypePropertyId,
            UIA_AutomationIdPropertyId,
            UIA_ClassNamePropertyId,
            UIA_BoundingRectanglePropertyId,
            UIA_IsEnabledPropertyId,
            UIA_IsOffscreenPropertyId,
        ] {
            request.AddProperty(property)?;
        }
        request.SetTreeScope(TreeScope_Subtree)?;
        root.BuildUpdatedCache(&request).unwrap_or(root)
    };

    let mut out = Vec::new();
    let mut cache = Vec::new();
    let mut state = WalkState {
        out: &mut out,
        cache: &mut cache,
        used: HashMap::new(),
        emitted: 0,
        visited: 0,
        max_visited: 1200,
        budget: max_elements.max(1),
        deadline: std::time::Instant::now() + std::time::Duration::from_millis(2500),
    };
    unsafe { walk(&walker, &root, &request, hwnd, 0, max_depth, &mut state) };

    CACHE.with(|slot| {
        let mut map = slot.borrow_mut();
        // 同一窗口的旧句柄先清掉，避免 id 指向已经消失的控件
        map.retain(|_, (cached_hwnd, _, _)| *cached_hwnd != hwnd);
        for (id, element, control_type) in cache {
            map.insert(id, (hwnd, element, control_type));
        }
    });
    Ok(out)
}

/// 观察主窗口，并把它的弹出层（菜单、下拉框、对话框）一起合并进来。
///
/// 这些浮层通常是独立顶层窗口，只读主窗口会漏掉它们；合并后模型仍然用一个
/// `observe` 就能看到当前所有可点的东西。
pub fn observe(
    hwnd: i64,
    max_depth: u32,
    max_elements: usize,
    include_popups: bool,
) -> Result<Vec<Node>> {
    let mut all = observe_window(hwnd, max_depth, max_elements)?;
    if !include_popups {
        return Ok(all);
    }
    for popup in super::window::related_popups(hwnd) {
        if all.len() >= max_elements {
            break;
        }
        let remaining = max_elements - all.len();
        let mut sub = match observe_with_walker(
            popup.hwnd,
            max_depth.saturating_sub(1).max(1),
            remaining,
            true,
        ) {
            Ok(nodes) => nodes,
            Err(_) => continue,
        };
        if sub.is_empty() {
            continue;
        }
        // 标成 Popup 并缩进一层，让模型看出这是主窗口之外的浮层
        if let Some(root) = sub.first_mut() {
            root.control_type = "Popup";
        }
        for node in sub.iter_mut() {
            node.depth += 1;
        }
        all.extend(sub);
    }
    Ok(all)
}

pub fn render(nodes: &[Node]) -> String {
    let mut text = String::new();
    for node in nodes {
        let indent = "  ".repeat(node.depth.min(8) as usize);
        let mut line = format!(
            "{indent}{} {} \"{}\"",
            node.id, node.control_type, node.name
        );
        if !node.automation_id.is_empty() {
            line.push_str(&format!(" aid={}", node.automation_id));
        }
        line.push_str(&format!(
            " @{},{},{}x{}",
            node.rect.left,
            node.rect.top,
            node.rect.right - node.rect.left,
            node.rect.bottom - node.rect.top
        ));
        if !node.enabled {
            line.push_str(" 禁用");
        }
        text.push_str(&line);
        text.push('\n');
    }
    text
}

fn lookup(id: &str) -> Result<(i64, IUIAutomationElement, i32)> {
    CACHE
        .with(|slot| slot.borrow().get(id).cloned())
        .ok_or_else(|| anyhow!("找不到控件 {id}：先调用 observe 重新获取，或该控件已消失"))
}

pub fn rect_of(id: &str) -> Result<(i64, RECT)> {
    let (hwnd, element, _) = lookup(id)?;
    let rect = unsafe { element.CurrentBoundingRectangle()? };
    Ok((hwnd, rect))
}

pub fn focus_element(id: &str) -> Result<()> {
    let (_, element, _) = lookup(id)?;
    unsafe {
        element.SetFocus()?;
    }
    Ok(())
}

/// 优先用控件自身暴露的动作（Invoke / Toggle / Select / Expand / 默认动作），
/// 都不支持时才退回坐标点击。模型只需要说“点它”，不用关心走哪条路。
///
/// 菜单栏项（MenuItem / SplitButton）例外：它们的 Invoke 和 Expand 都会返回成功
/// 但界面毫无变化，所以这一类直接走真实点击。
pub fn activate(id: &str) -> Result<&'static str> {
    let (_, element, control_type) = lookup(id)?;
    // 菜单项直接真实点击：WinUI 的菜单栏项对 Invoke / Expand 都返回成功，
    // 但界面不会变化，只有真的点一下才会展开
    let prefers_real_click = matches!(control_type, 50011 | 50031);
    if !prefers_real_click {
        unsafe {
            if let Ok(pattern) = element
                .GetCurrentPattern(UIA_InvokePatternId)
                .and_then(|unknown| Ok(unknown.cast::<IUIAutomationInvokePattern>()?))
            {
                if pattern.Invoke().is_ok() {
                    return Ok("invoke");
                }
            }
            if let Ok(pattern) = element
                .GetCurrentPattern(UIA_TogglePatternId)
                .and_then(|unknown| Ok(unknown.cast::<IUIAutomationTogglePattern>()?))
            {
                if pattern.Toggle().is_ok() {
                    return Ok("toggle");
                }
            }
            if let Ok(pattern) = element
                .GetCurrentPattern(UIA_SelectionItemPatternId)
                .and_then(|unknown| Ok(unknown.cast::<IUIAutomationSelectionItemPattern>()?))
            {
                if pattern.Select().is_ok() {
                    return Ok("select");
                }
            }
            if let Ok(pattern) = element
                .GetCurrentPattern(UIA_ExpandCollapsePatternId)
                .and_then(|unknown| Ok(unknown.cast::<IUIAutomationExpandCollapsePattern>()?))
            {
                if pattern.Expand().is_ok() {
                    return Ok("expand");
                }
            }
            if let Ok(pattern) = element
                .GetCurrentPattern(UIA_LegacyIAccessiblePatternId)
                .and_then(|unknown| Ok(unknown.cast::<IUIAutomationLegacyIAccessiblePattern>()?))
            {
                if pattern.DoDefaultAction().is_ok() {
                    return Ok("default-action");
                }
            }
        }
    }
    // 兜底：点控件中心
    let (hwnd, rect) = rect_of(id)?;
    if rect.right <= rect.left || rect.bottom <= rect.top {
        return Err(anyhow!("控件 {id} 没有可见区域，且不支持任何语义动作"));
    }
    let x = (rect.left + rect.right) / 2;
    let y = (rect.top + rect.bottom) / 2;
    super::window::focus(hwnd)?;
    super::input::click(x, y, "left", 1)?;
    Ok("click-center")
}

/// 回读控件内容，确认写入是否真的生效。
///
/// 只看 ValuePattern 不够：WinUI 的文档控件会记住 SetValue 传进去的值，
/// 但界面和文件都没变。所以再读一次 TextPattern 的正文做交叉验证。
unsafe fn content_matches(element: &IUIAutomationElement, text: &str) -> bool {
    // 文档正文以 TextPattern 为准：ValuePattern 在 WinUI 的文档控件上会撒谎，
    // 它记得 SetValue 传进去的值，但界面和文件都没变
    if let Ok(pattern) = element
        .GetCurrentPattern(UIA_TextPatternId)
        .and_then(|unknown| Ok(unknown.cast::<IUIAutomationTextPattern>()?))
    {
        if let Ok(range) = pattern.DocumentRange() {
            if let Ok(body) = range.GetText(-1) {
                return body.to_string().contains(text);
            }
        }
    }
    if let Ok(pattern) = element
        .GetCurrentPattern(UIA_ValuePatternId)
        .and_then(|unknown| Ok(unknown.cast::<IUIAutomationValuePattern>()?))
    {
        return pattern
            .CurrentValue()
            .map(|value| value.to_string() == text)
            .unwrap_or(false);
    }
    false
}

/// 直接把值写进控件。没有 ValuePattern 时退回“聚焦 + 全选 + 输入”。
///
/// 不少控件（记事本的正文区、部分 RichEdit）会接受 SetValue 却不真的改内容，
/// 所以写完要回读一次，对不上就走输入通道，不让调用方去试第二次。
pub fn set_value(id: &str, text: &str) -> Result<(&'static str, Option<bool>)> {
    let (hwnd, element, control_type) = lookup(id)?;
    // 文档类控件（记事本正文、Word 编辑区）一律走键盘：它们的 ValuePattern
    // 会接受 SetValue 并把值记在属性里，但正文和文件都不会变，无法可靠校验
    let is_document = control_type == 50030;
    if !is_document {
        unsafe {
            if let Ok(pattern) = element
                .GetCurrentPattern(UIA_ValuePatternId)
                .and_then(|unknown| Ok(unknown.cast::<IUIAutomationValuePattern>()?))
            {
                let read_only = pattern
                    .CurrentIsReadOnly()
                    .map(|value| value.as_bool())
                    .unwrap_or(false);
                if !read_only
                    && pattern.SetValue(&BSTR::from(text)).is_ok()
                    && content_matches(&element, text)
                {
                    return Ok(("value-pattern", Some(true)));
                }
            }
        }
    }
    // 键盘通道要求目标窗口在前台。SetForegroundWindow 经常被系统拒绝，
    // 所以在控件上真点一下：真实点击一定会把焦点带过去
    let rect = unsafe { element.CurrentBoundingRectangle().unwrap_or_default() };
    super::window::focus(hwnd)?;
    sleep_ms(80);
    if rect.right > rect.left && rect.bottom > rect.top {
        let x = (rect.left + rect.right) / 2;
        let y = (rect.top + rect.bottom) / 2;
        super::input::click(x, y, "left", 1)?;
        sleep_ms(80);
    } else {
        focus_element(id)?;
        sleep_ms(60);
    }
    super::input::press_keys("ctrl+a")?;
    sleep_ms(30);
    super::input::type_text(text)?;
    if !super::window::is_foreground(hwnd) {
        // 常见于窗口在别的虚拟桌面、或被系统前台锁挡住
        return Err(anyhow!(
            "键盘输入没能送达：目标窗口不在前台。请先把它切到当前桌面并激活，或改用 click + type_text"
        ));
    }
    let verified = unsafe {
        lookup(id)
            .ok()
            .map(|(_, element, _)| content_matches(&element, text))
            .unwrap_or(true)
    };
    // 文档类控件的回读经常滞后或不准（刚输入完 UIA 还是旧内容），
    // 这种情况不下结论，让调用方用截图确认
    let verified = if is_document { None } else { Some(verified) };
    Ok(("focus+type", verified))
}

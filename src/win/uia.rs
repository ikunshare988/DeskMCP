use std::cell::RefCell;
use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};

use anyhow::{anyhow, bail, Result};
use windows::core::{BSTR, Interface};
use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED,
};
use windows::Win32::UI::Accessibility::{
    CUIAutomation, IUIAutomation, IUIAutomation2, IUIAutomationCacheRequest, IUIAutomationElement,
    IUIAutomationExpandCollapsePattern, IUIAutomationInvokePattern,
    IUIAutomationLegacyIAccessiblePattern, IUIAutomationRangeValuePattern,
    IUIAutomationScrollItemPattern, IUIAutomationScrollPattern,
    IUIAutomationSelectionItemPattern,
    IUIAutomationTogglePattern, IUIAutomationTextPattern, IUIAutomationValuePattern,
    IUIAutomationVirtualizedItemPattern, IUIAutomationWindowPattern,
    TreeScope_Subtree, UIA_AutomationIdPropertyId, UIA_BoundingRectanglePropertyId,
    UIA_ClassNamePropertyId, UIA_ControlTypePropertyId, UIA_ExpandCollapsePatternId,
    UIA_InvokePatternId, UIA_IsEnabledPropertyId, UIA_IsOffscreenPropertyId,
    UIA_LegacyIAccessiblePatternId, UIA_NamePropertyId, UIA_RangeValuePatternId,
    UIA_ScrollItemPatternId, UIA_ScrollPatternId, UIA_SelectionItemPatternId,
    UIA_TextPatternId, UIA_TogglePatternId, UIA_ValuePatternId,
    UIA_VirtualizedItemPatternId, UIA_WindowPatternId, UIA_PATTERN_ID,
};

use super::{sleep_ms, to_hwnd};

/// 缓存下来的控件句柄，附带 observe 当时记录的身份。
///
/// 身份字段不是给模型看的，而是下次用这个 id 之前的比对基准：UIA 的元素句柄会随
/// 界面变化被回收复用，只凭 id 去点，可能点到同一个位置上换过去的另一个控件。
#[derive(Clone)]
struct Cached {
    hwnd: i64,
    element: IUIAutomationElement,
    control_type: i32,
    name: String,
}

thread_local! {
    static AUTOMATION: RefCell<Option<IUIAutomation>> = const { RefCell::new(None) };
    // 元素句柄按 id 缓存，模型可以跨多次调用引用同一个控件，不必每次重新观察
    static CACHE: RefCell<HashMap<String, Cached>> = RefCell::new(HashMap::new());
}

/// 退出前释放缓存的控件句柄。
///
/// 这些 `IUIAutomationElement` 如果留到进程退出、由 thread_local 析构去 Release，
/// 会撞进 OLE32 的线程分离路径互相等待：进程永远不退出，只剩一个卡在
/// `Wait/UserRequest` 的线程。必须在读循环还活着的时候主动清掉。
pub fn clear_cache() {
    CACHE.with(|slot| slot.borrow_mut().clear());
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
        filter: None,
        include_offscreen: false,
        truncated: false,
        stop_reason: None,
    };
    let started = std::time::Instant::now();
    unsafe { walk(&walker, &root, &request, hwnd, 0, 12, &mut state) };
    let visited = state.visited;
    let truncated = state.truncated;
    let reason = state.stop_reason;
    drop(state);
    let _ = writeln!(
        out,
        "walk 结束: 输出 {} 个控件，访问 {} 个节点{}，耗时 {}ms",
        nodes.len(),
        visited,
        match (truncated, reason) {
            (true, Some(reason)) => format!("，已截断（{reason}）"),
            (true, None) => "，已截断".to_string(),
            _ => String::new(),
        },
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
            Ok(observation) => {
                let _ = writeln!(out, "  → {} 个控件", observation.nodes.len());
                for line in render(&observation.nodes).lines().take(12) {
                    let _ = writeln!(out, "    {line}");
                }
            }
            Err(error) => {
                let _ = writeln!(out, "  → 读取失败: {error}");
            }
        }
    }
}

/// 控件筛选条件：全部可选，同时给出时按 AND 组合。
#[derive(Default)]
pub struct Filter {
    pub name: Option<String>,
    pub exact_name: bool,
    pub automation_id: Option<String>,
    pub control_type: Option<i32>,
    pub class_name: Option<String>,
    pub enabled: Option<bool>,
    pub include_offscreen: bool,
}

impl Filter {
    fn matches(&self, name: &str, aid: &str, control_type: i32, class: &str, enabled: bool) -> bool {
        if let Some(want) = &self.name {
            let hay = name.to_lowercase();
            let needle = want.to_lowercase();
            let hit = if self.exact_name { hay == needle } else { hay.contains(&needle) };
            if !hit {
                return false;
            }
        }
        if let Some(want) = &self.automation_id {
            if aid != want {
                return false;
            }
        }
        if let Some(want) = self.control_type {
            if control_type != want {
                return false;
            }
        }
        if let Some(want) = &self.class_name {
            if !class.to_lowercase().contains(&want.to_lowercase()) {
                return false;
            }
        }
        if let Some(want) = self.enabled {
            if enabled != want {
                return false;
            }
        }
        true
    }
}

/// 一次遍历的结果。`truncated` 用来如实告诉调用方「我看到的不一定是全部」，
/// 而不是让模型以为没有别的控件了。
pub struct Observation {
    pub nodes: Vec<Node>,
    pub visited: usize,
    pub truncated: bool,
    pub stop_reason: Option<&'static str>,
}

struct WalkState<'a> {
    out: &'a mut Vec<Node>,
    cache: &'a mut Vec<(String, Cached)>,
    used: HashMap<String, u32>,
    emitted: usize,
    visited: usize,
    max_visited: usize,
    budget: usize,
    deadline: std::time::Instant,
    /// 给出时只收集命中条件的控件；为空时按 observe 的规则输出
    filter: Option<&'a Filter>,
    include_offscreen: bool,
    truncated: bool,
    stop_reason: Option<&'static str>,
}

impl WalkState<'_> {
    /// 触到三道上限中的哪一道；没触到返回 None。
    fn stop_reason(&self) -> Option<&'static str> {
        if self.emitted >= self.budget {
            Some("元素上限")
        } else if self.visited >= self.max_visited {
            Some("节点上限")
        } else if std::time::Instant::now() >= self.deadline {
            Some("时间上限")
        } else {
            None
        }
    }

    fn exhausted(&self) -> bool {
        self.stop_reason().is_some()
    }

    /// 在「还有兄弟节点要先处理」的位置收工，才叫截断。
    fn mark_truncated(&mut self) {
        if let Some(reason) = self.stop_reason() {
            self.truncated = true;
            self.stop_reason = Some(reason);
        }
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
    if offscreen && !state.include_offscreen {
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

    let emit = match state.filter {
        // find：只收命中条件的，不再套「无名容器是否有意义」那套规则
        Some(filter) => {
            filter.matches(&name, &automation_id, control_type, &class, enabled)
        }
        None => !name.trim().is_empty() || worthwhile_without_name(control_type),
    };
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
        state.cache.push((
            id,
            Cached {
                hwnd,
                element: element.clone(),
                control_type,
                name,
            },
        ));
        state.emitted += 1;
    }

    let mut child = walker.GetFirstChildElementBuildCache(element, request).ok();
    while let Some(current) = child {
        walk(walker, &current, request, hwnd, depth + 1, max_depth, state);
        let next = walker
            .GetNextSiblingElementBuildCache(&current, request)
            .ok();
        if state.exhausted() {
            // 只有「后面确实还有兄弟节点没处理」才算截断，
            // 否则会把刚好在最后一个节点上收工误报成截断
            if next.is_some() {
                state.mark_truncated();
            }
            break;
        }
        child = next;
    }
}

/// 遍历单个窗口的控件树，返回压缩后的节点列表，并把控件句柄缓存起来。
///
/// 三条护栏防止被卡死的窗口拖住：缓存请求（一次 IPC 取全部属性）、
/// 访问节点上限、2.5 秒墙钟上限。
pub fn observe_window(hwnd: i64, max_depth: u32, max_elements: usize) -> Result<Observation> {
    observe_with_walker(hwnd, max_depth, max_elements, false)
}

/// 遍历的公共实现。root 由调用方解析（窗口根节点，或某个控件的子树）。
struct WalkOptions<'a> {
    /// 用于 id 哈希归属的窗口句柄
    hwnd: i64,
    max_depth: u32,
    max_elements: usize,
    raw: bool,
    filter: Option<&'a Filter>,
    include_offscreen: bool,
    /// observe 会清掉该窗口的旧句柄；find 只补充命中的，不动已有缓存
    purge_cache: bool,
}

fn walk_from(root: &IUIAutomationElement, opt: &WalkOptions<'_>) -> Result<Observation> {
    let automation = automation()?;
    let walker = unsafe {
        if opt.raw {
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
        root.BuildUpdatedCache(&request).unwrap_or_else(|_| root.clone())
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
        budget: opt.max_elements.max(1),
        deadline: std::time::Instant::now() + std::time::Duration::from_millis(2500),
        filter: opt.filter,
        include_offscreen: opt.include_offscreen,
        truncated: false,
        stop_reason: None,
    };
    unsafe { walk(&walker, &root, &request, opt.hwnd, 0, opt.max_depth, &mut state) };
    let visited = state.visited;
    let truncated = state.truncated;
    let stop_reason = state.stop_reason;

    CACHE.with(|slot| {
        let mut map = slot.borrow_mut();
        if opt.purge_cache {
            // 同一窗口的旧句柄先清掉，避免 id 指向已经消失的控件
            map.retain(|_, cached| cached.hwnd != opt.hwnd);
        }
        for (id, cached) in cache {
            map.insert(id, cached);
        }
    });

    Ok(Observation {
        nodes: out,
        visited,
        truncated,
        stop_reason,
    })
}

/// raw=true 用 RawViewWalker：WinUI 的弹出菜单只出现在原始视图里，控件视图会过滤掉。
fn observe_with_walker(
    hwnd: i64,
    max_depth: u32,
    max_elements: usize,
    raw: bool,
) -> Result<Observation> {
    let root = element_from_hwnd(hwnd)?;
    walk_from(
        &root,
        &WalkOptions {
            hwnd,
            max_depth,
            max_elements,
            raw,
            filter: None,
            include_offscreen: false,
            purge_cache: true,
        },
    )
}

/// 按条件查找控件。
///
/// 与 observe 的区别是**不 dump 整棵树**：模型可以直接问「保存按钮在哪」，
/// 而不是先把整棵树读回来自己找。这也绕开了「一次只能输出 max_elements 个元素」
/// 的限制——命中多少个就回多少个。
///
/// 命中的控件会写进缓存，返回的 id 可以直接交给 click_element / read / set_value。
/// 与 observe 不同，find 不会清掉该窗口已有的缓存，其它 id 仍然可用。
pub fn find(
    hwnd: i64,
    filter: &Filter,
    within: Option<&str>,
    max_depth: u32,
    limit: usize,
) -> Result<Observation> {
    let (root, walk_hwnd) = match within {
        Some(id) => {
            let cached = lookup(id)?;
            (cached.element.clone(), cached.hwnd)
        }
        None => (element_from_hwnd(hwnd)?, hwnd),
    };
    walk_from(
        &root,
        &WalkOptions {
            hwnd: walk_hwnd,
            max_depth,
            max_elements: limit,
            raw: false,
            filter: Some(filter),
            include_offscreen: filter.include_offscreen,
            purge_cache: false,
        },
    )
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
) -> Result<Observation> {
    let mut main = observe_window(hwnd, max_depth, max_elements)?;
    if !include_popups {
        return Ok(main);
    }
    for popup in super::window::related_popups(hwnd) {
        if main.nodes.len() >= max_elements {
            break;
        }
        let remaining = max_elements - main.nodes.len();
        let mut sub = match observe_with_walker(
            popup.hwnd,
            max_depth.saturating_sub(1).max(1),
            remaining,
            true,
        ) {
            Ok(observation) => observation,
            Err(_) => continue,
        };
        if sub.nodes.is_empty() {
            continue;
        }
        // 标成 Popup 并缩进一层，让模型看出这是主窗口之外的浮层
        if let Some(root) = sub.nodes.first_mut() {
            root.control_type = "Popup";
        }
        for node in sub.nodes.iter_mut() {
            node.depth += 1;
        }
        main.visited += sub.visited;
        main.truncated |= sub.truncated;
        if main.stop_reason.is_none() {
            main.stop_reason = sub.stop_reason;
        }
        main.nodes.extend(sub.nodes);
    }
    Ok(main)
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

/// 取出 id 对应的控件，并在使用前确认它还是 observe 时的那个控件。
fn lookup(id: &str) -> Result<Cached> {
    let cached = CACHE
        .with(|slot| slot.borrow().get(id).cloned())
        .ok_or_else(|| anyhow!("找不到控件 {id}：先调用 observe 重新获取，或该控件已消失"))?;

    // 身份比对必须走 Current*：Cached* 是 observe 那一刻存下来的本地副本，
    // 拿它跟它自己比永远相等，读不出任何变化。
    //
    // Current* 失败就说明这个句柄已经不可达（控件被销毁，或从树上摘掉换成了新的）。
    // 这里不能用 Cached* 兜底——那会把「已失效」伪装成「一切正常」，让调用方
    // 照着过期的位置去操作。
    let (type_result, name_result) = unsafe {
        (
            cached.element.CurrentControlType(),
            cached.element.CurrentName(),
        )
    };
    let (Ok(live_type), Ok(live_name)) = (type_result, name_result) else {
        return Err(anyhow!("控件 {id} 已失效（控件已被移除或重建）：请重新 observe"));
    };

    // id 只保证「observe 那一刻」指向这个控件。界面变了以后 UIA 会把元素句柄回收给
    // 同一个位置上的另一个控件，照着旧 id 操作就会作用到错的东西上（比如列表换了
    // 内容之后点到另一行）。对不上就明确报错，不猜也不乱点。
    if live_type.0 != cached.control_type {
        return Err(anyhow!(
            "控件 {id} 已经不是 {} 了（现在是 {}）：界面已变化，请重新 observe",
            control_type_name(cached.control_type),
            control_type_name(live_type.0)
        ));
    }
    let live_name = live_name.to_string();
    if live_name != cached.name {
        return Err(anyhow!(
            "控件 {id} 已不是 observe 时的那个（当时 {:?}，现在 {:?}）：界面已变化，请重新 observe",
            cached.name,
            live_name
        ));
    }
    Ok(cached)
}

pub fn rect_of(id: &str) -> Result<(i64, RECT)> {
    let cached = lookup(id)?;
    let rect = unsafe { cached.element.CurrentBoundingRectangle()? };
    Ok((cached.hwnd, rect))
}

pub fn focus_element(id: &str) -> Result<()> {
    let cached = lookup(id)?;
    unsafe {
        cached.element.SetFocus()?;
    }
    Ok(())
}

/// 取一个模式；控件不支持时返回 None（用于「按顺序试」）。
fn try_pattern<T: Interface + Clone>(element: &IUIAutomationElement, id: UIA_PATTERN_ID) -> Option<T> {
    unsafe { element.GetCurrentPattern(id).ok()?.cast::<T>().ok() }
}

/// 取一个模式；不支持时报错（用于「明确要求某个动作」）。
fn get_pattern<T: Interface + Clone>(
    element: &IUIAutomationElement,
    id: UIA_PATTERN_ID,
    what: &str,
) -> Result<T> {
    unsafe {
        element
            .GetCurrentPattern(id)
            .map_err(|_| anyhow!("控件不支持{what}"))?
            .cast::<T>()
            .map_err(|_| anyhow!("控件不支持{what}"))
    }
}

/// 控件可达性。
enum Reach {
    /// 本来就看得见
    Visible,
    /// 做了一步操作才看见，带上做法
    Moved(&'static str),
    /// 看不见，而且没有可用的滚动能力
    Stuck,
}

/// 让控件变得可达。
///
/// 两件事必须处理，否则模型拿到的 id 点下去只会得到「没有可见区域」：
/// - 虚拟化列表里还没实体化的项 → `VirtualizedItem.Realize`
/// - 滚出视野的行 → `ScrollItem.ScrollIntoView`
fn ensure_reachable(cached: &Cached) -> Reach {
    let element = &cached.element;
    let offscreen = unsafe { element.CurrentIsOffscreen() }
        .map(|value| value.as_bool())
        .unwrap_or(false);
    let rect = unsafe { element.CurrentBoundingRectangle() }.unwrap_or_default();
    let empty = rect.right <= rect.left || rect.bottom <= rect.top;
    if !offscreen && !empty {
        return Reach::Visible;
    }

    let mut did = None;
    if let Some(pattern) =
        try_pattern::<IUIAutomationVirtualizedItemPattern>(element, UIA_VirtualizedItemPatternId)
    {
        if unsafe { pattern.Realize() }.is_ok() {
            did = Some("realize");
        }
    }
    if let Some(pattern) =
        try_pattern::<IUIAutomationScrollItemPattern>(element, UIA_ScrollItemPatternId)
    {
        if unsafe { pattern.ScrollIntoView() }.is_ok() {
            sleep_ms(80);
            did = Some("scroll-into-view");
        }
    }
    match did {
        Some(how) => Reach::Moved(how),
        None => Reach::Stuck,
    }
}

/// 把控件滚进视野（语义滚动，不靠鼠标落在容器上）。
pub fn scroll_into_view(id: &str) -> Result<String> {
    let cached = lookup(id)?;
    match ensure_reachable(&cached) {
        Reach::Visible => Ok("本来就在视野内".to_string()),
        Reach::Moved(how) => Ok(how.to_string()),
        Reach::Stuck => Err(anyhow!(
            "控件 {id} 不可见，而且它和它的容器都没有滚动能力，滚不进来"
        )),
    }
}

/// 把控件上的一个动作包一层：失败时带上「对谁、做什么」，而不是只丢一个裸 HRESULT。
fn act_on<T>(id: &str, what: &str, done: windows::core::Result<T>) -> Result<T> {
    done.map_err(|error| anyhow!("{id} {what}失败：{error}"))
}

/// 让容器自己滚动（ScrollPattern 设百分比），不依赖鼠标位置。
pub fn scroll_container(id: &str, direction: &str, amount: i64) -> Result<String> {
    let cached = lookup(id)?;
    let pattern: IUIAutomationScrollPattern =
        get_pattern(&cached.element, UIA_ScrollPatternId, "滚动（Scroll）")?;
    let now_h = unsafe { pattern.CurrentHorizontalScrollPercent() }.unwrap_or(-1.0);
    let now_v = unsafe { pattern.CurrentVerticalScrollPercent() }.unwrap_or(-1.0);
    if now_h < 0.0 && now_v < 0.0 {
        // 两个轴都没有百分比：这类容器只认滚轮，别拿一个裸 HRESULT 去糊调用方
        bail!(
            "控件 {id} 不支持按百分比滚动（Scroll 没提供 ScrollPercent）。\
             改用滚轮：scroll {{x, y, amount}}，或先用 scroll {{item: <要滚到的控件 id>}}"
        );
    }
    let step = (amount.max(1) as f64) * 10.0;
    let clamp = |value: f64| value.max(0.0).min(100.0);
    let (horizontal, vertical) = match direction {
        "left" => (if now_h < 0.0 { -1.0 } else { clamp(now_h - step) }, -1.0),
        "right" => (if now_h < 0.0 { -1.0 } else { clamp(now_h + step) }, -1.0),
        "up" => (-1.0, if now_v < 0.0 { -1.0 } else { clamp(now_v - step) }),
        "down" => (-1.0, if now_v < 0.0 { -1.0 } else { clamp(now_v + step) }),
        other => bail!("direction 只能是 up / down / left / right，收到 {other}"),
    };
    act_on(id, "滚动", unsafe { pattern.SetScrollPercent(horizontal, vertical) })?;
    sleep_ms(80);
    let after_h = unsafe { pattern.CurrentHorizontalScrollPercent() }.unwrap_or(-1.0);
    let after_v = unsafe { pattern.CurrentVerticalScrollPercent() }.unwrap_or(-1.0);
    Ok(format!("滚动到 水平 {after_h:.0}% / 垂直 {after_v:.0}%"))
}

/// 显式执行一个动作，返回实际用到的能力名。
///
/// 与 `click_element` 的分工：`click_element` 是「你猜我要干什么」（自动降级链），
/// `invoke` 是「我知道要干什么」——滑块设值、滚到某项、展开/收起树节点、
/// 最大化/关闭窗口这类精确动作只能靠它。
pub fn invoke(id: &str, action: &str, value: Option<&str>) -> Result<String> {
    use windows::Win32::UI::Accessibility::{
        WindowVisualState_Maximized, WindowVisualState_Minimized, WindowVisualState_Normal,
    };

    let cached = lookup(id)?;
    let element = &cached.element;
    match action {
        "focus" => {
            unsafe { element.SetFocus()? };
            Ok("focus".to_string())
        }
        "scroll_into_view" => scroll_into_view(id),
        "realize" => {
            let pattern: IUIAutomationVirtualizedItemPattern = get_pattern(
                element,
                UIA_VirtualizedItemPatternId,
                "虚拟项实体化（VirtualizedItem）",
            )?;
            act_on(id, "实体化", unsafe { pattern.Realize() })?;
            Ok("realize".to_string())
        }
        "invoke" => {
            let pattern: IUIAutomationInvokePattern =
                get_pattern(element, UIA_InvokePatternId, "Invoke（通常是按钮）")?;
            act_on(id, "触发", unsafe { pattern.Invoke() })?;
            Ok("invoke".to_string())
        }
        "toggle" => {
            let pattern: IUIAutomationTogglePattern =
                get_pattern(element, UIA_TogglePatternId, "Toggle（通常是复选框）")?;
            act_on(id, "切换", unsafe { pattern.Toggle() })?;
            Ok("toggle".to_string())
        }
        "select" => {
            let pattern: IUIAutomationSelectionItemPattern =
                get_pattern(element, UIA_SelectionItemPatternId, "Select（列表项或页签）")?;
            act_on(id, "选中", unsafe { pattern.Select() })?;
            Ok("select".to_string())
        }
        "expand" | "collapse" => {
            let pattern: IUIAutomationExpandCollapsePattern = get_pattern(
                element,
                UIA_ExpandCollapsePatternId,
                "展开/收起（ExpandCollapse，通常是树节点或下拉框）",
            )?;
            if action == "expand" {
                act_on(id, "展开", unsafe { pattern.Expand() })?;
            } else {
                act_on(id, "收起", unsafe { pattern.Collapse() })?;
            }
            Ok(action.to_string())
        }
        "default_action" => {
            let pattern: IUIAutomationLegacyIAccessiblePattern = get_pattern(
                element,
                UIA_LegacyIAccessiblePatternId,
                "默认动作（LegacyIAccessible）",
            )?;
            act_on(id, "默认动作", unsafe { pattern.DoDefaultAction() })?;
            Ok("default-action".to_string())
        }
        "set_value" => {
            let text = value.ok_or_else(|| anyhow!("set_value 需要 value"))?;
            let pattern: IUIAutomationValuePattern =
                get_pattern(element, UIA_ValuePatternId, "写入值（Value）")?;
            act_on(id, "写入", unsafe { pattern.SetValue(&BSTR::from(text)) })?;
            Ok("value".to_string())
        }
        "set_range" => {
            let text = value.ok_or_else(|| anyhow!("set_range 需要 value，例如 \"60\""))?;
            let number: f64 = text
                .trim()
                .parse()
                .map_err(|_| anyhow!("set_range 的 value 需要是数字，收到 {text:?}"))?;
            let pattern: IUIAutomationRangeValuePattern = get_pattern(
                element,
                UIA_RangeValuePatternId,
                "设值（RangeValue，通常是滑块或进度条）",
            )?;
            act_on(id, "设值", unsafe { pattern.SetValue(number) })?;
            Ok("range-value".to_string())
        }
        "close" => {
            let pattern: IUIAutomationWindowPattern =
                get_pattern(element, UIA_WindowPatternId, "窗口操作（Window）")?;
            act_on(id, "关闭", unsafe { pattern.Close() })?;
            Ok("close".to_string())
        }
        "maximize" | "minimize" | "restore" => {
            let pattern: IUIAutomationWindowPattern =
                get_pattern(element, UIA_WindowPatternId, "窗口操作（Window）")?;
            let state = match action {
                "maximize" => WindowVisualState_Maximized,
                "minimize" => WindowVisualState_Minimized,
                _ => WindowVisualState_Normal,
            };
            act_on(id, action, unsafe { pattern.SetWindowVisualState(state) })?;
            Ok(action.to_string())
        }
        "click_center" => {
            let (hwnd, rect) = rect_of(id)?;
            if rect.right <= rect.left || rect.bottom <= rect.top {
                return Err(anyhow!("控件 {id} 没有可见区域，点不了中心"));
            }
            let x = (rect.left + rect.right) / 2;
            let y = (rect.top + rect.bottom) / 2;
            super::window::focus(hwnd)?;
            super::input::click(x, y, "left", 1)?;
            Ok("click-center".to_string())
        }
        other => Err(anyhow!(
            "未知动作：{other}。可用：focus / scroll_into_view / realize / invoke / toggle / \
             select / expand / collapse / default_action / set_value / set_range / close / \
             maximize / minimize / restore / click_center"
        )),
    }
}

/// 优先用控件自身暴露的动作（Invoke / Toggle / Select / Expand / 默认动作），
/// 都不支持时才退回坐标点击。模型只需要说“点它”，不用关心走哪条路。
///
/// 菜单栏项（MenuItem / SplitButton）例外：它们的 Invoke 和 Expand 都会返回成功
/// 但界面毫无变化，所以这一类直接走真实点击。
pub fn activate(id: &str) -> Result<String> {
    let cached = lookup(id)?;
    // 虚拟化列表里没实体化的项、滚出视野的行：先让它可达，否则下面什么都点不到
    let reach = ensure_reachable(&cached);
    let how = activate_now(id, &cached)?;
    Ok(match reach {
        Reach::Moved(step) => format!("{step}+{how}"),
        _ => how,
    })
}

/// 真正挑动作的部分：按可靠性从高到低试一遍。
fn activate_now(id: &str, cached: &Cached) -> Result<String> {
    let element = &cached.element;
    // 菜单项直接真实点击：WinUI 的菜单栏项对 Invoke / Expand 都返回成功，
    // 但界面不会变化，只有真的点一下才会展开
    let prefers_real_click = matches!(cached.control_type, 50011 | 50031);
    if !prefers_real_click {
        if let Some(pattern) =
            try_pattern::<IUIAutomationInvokePattern>(element, UIA_InvokePatternId)
        {
            if unsafe { pattern.Invoke() }.is_ok() {
                return Ok("invoke".to_string());
            }
        }
        if let Some(pattern) =
            try_pattern::<IUIAutomationTogglePattern>(element, UIA_TogglePatternId)
        {
            if unsafe { pattern.Toggle() }.is_ok() {
                return Ok("toggle".to_string());
            }
        }
        if let Some(pattern) =
            try_pattern::<IUIAutomationSelectionItemPattern>(element, UIA_SelectionItemPatternId)
        {
            if unsafe { pattern.Select() }.is_ok() {
                return Ok("select".to_string());
            }
        }
        if let Some(pattern) =
            try_pattern::<IUIAutomationExpandCollapsePattern>(element, UIA_ExpandCollapsePatternId)
        {
            if unsafe { pattern.Expand() }.is_ok() {
                return Ok("expand".to_string());
            }
        }
        if let Some(pattern) = try_pattern::<IUIAutomationLegacyIAccessiblePattern>(
            element,
            UIA_LegacyIAccessiblePatternId,
        ) {
            if unsafe { pattern.DoDefaultAction() }.is_ok() {
                return Ok("default-action".to_string());
            }
        }
    }
    // 兜底：点控件中心
    let (hwnd, rect) = rect_of(id)?;
    if rect.right <= rect.left || rect.bottom <= rect.top {
        return Err(anyhow!(
            "控件 {id} 没有可见区域，也不支持任何语义动作。它可能是虚拟化列表里还没实体化的项，\
             或者需要先把它的容器滚过去。"
        ));
    }
    let x = (rect.left + rect.right) / 2;
    let y = (rect.top + rect.bottom) / 2;
    super::window::focus(hwnd)?;
    super::input::click(x, y, "left", 1)?;
    Ok("click-center".to_string())
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
    let cached = lookup(id)?;
    let hwnd = cached.hwnd;
    let element = &cached.element;
    let control_type = cached.control_type;
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
                    && content_matches(element, text)
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
            .map(|cached| content_matches(&cached.element, text))
            .unwrap_or(true)
    };
    // 文档类控件的回读经常滞后或不准（刚输入完 UIA 还是旧内容），
    // 这种情况不下结论，让调用方用截图确认
    let verified = if is_document { None } else { Some(verified) };
    Ok(("focus+type", verified))
}

/// 把控件值压成一小段再回给模型。
///
/// 两件事必须挡住：
/// - 密码框的内容一律不回显（UIA 会把密码原文放在 ValuePattern 里）；
/// - 文档类控件（记事本正文、Word 编辑区）的正文可能非常长，也可能含密码/令牌/密钥。
///   这里只报字符数，不回显内容。
const VALUE_PREVIEW_LIMIT: usize = 120;

fn preview(text: &str, control_type: i32, is_password: bool) -> String {
    let count = text.chars().count();
    if is_password {
        return format!("<密码框内容已隐藏，{count} 字符>");
    }
    if control_type == 50030 {
        return format!("<文档正文 {count} 字符，已省略>");
    }
    if count <= VALUE_PREVIEW_LIMIT {
        return format!("{text:?}");
    }
    let head: String = text.chars().take(VALUE_PREVIEW_LIMIT).collect();
    format!("{head:?}…（共 {count} 字符）")
}

/// 把 "Button" / "button" 之类的名字还原成控件类型 id，供过滤条件使用。
pub fn control_type_from_name(name: &str) -> Option<i32> {
    let want = name.trim().to_lowercase();
    if want.is_empty() {
        return None;
    }
    if let Ok(number) = want.parse::<i32>() {
        return Some(number);
    }
    (50000..=50040).find(|id| control_type_name(*id).to_lowercase() == want)
}

/// 读一个控件当前的各项状态，拼成一行紧凑文本。
///
/// 这是「不靠截图验证」的主力：点完之后读一下值/开关/选中/展开状态，
/// 比再截一张整屏图便宜三个数量级，也不需要模型做任何坐标换算。
pub fn read_state(id: &str) -> Result<String> {
    let cached = lookup(id)?;
    let element = &cached.element;
    let mut parts = vec![format!(
        "{id} {} \"{}\"",
        control_type_name(cached.control_type),
        cached.name
    )];

    unsafe {
        if let Ok(rect) = element.CurrentBoundingRectangle() {
            parts.push(format!(
                "@{},{},{}x{}",
                rect.left,
                rect.top,
                rect.right - rect.left,
                rect.bottom - rect.top
            ));
        }
        if let Ok(value) = element.CurrentIsEnabled() {
            parts.push(format!("enabled={}", value.as_bool()));
        }
        if let Ok(value) = element.CurrentIsOffscreen() {
            parts.push(format!("offscreen={}", value.as_bool()));
        }
        if let Ok(value) = element.CurrentHasKeyboardFocus() {
            parts.push(format!("focus={}", value.as_bool()));
        }
        if let Ok(pattern) = element
            .GetCurrentPattern(UIA_ValuePatternId)
            .and_then(|unknown| Ok(unknown.cast::<IUIAutomationValuePattern>()?))
        {
            let hidden = element
                .CurrentIsPassword()
                .map(|value| value.as_bool())
                .unwrap_or(false);
            let text = pattern
                .CurrentValue()
                .map(|value| value.to_string())
                .unwrap_or_default();
            parts.push(format!("value={}", preview(&text, cached.control_type, hidden)));
            if let Ok(flag) = pattern.CurrentIsReadOnly() {
                parts.push(format!("readonly={}", flag.as_bool()));
            }
        }
        if let Ok(pattern) = element
            .GetCurrentPattern(UIA_TogglePatternId)
            .and_then(|unknown| Ok(unknown.cast::<IUIAutomationTogglePattern>()?))
        {
            if let Ok(state) = pattern.CurrentToggleState() {
                parts.push(format!(
                    "toggle={}",
                    match state.0 {
                        0 => "Off",
                        1 => "On",
                        _ => "Indeterminate",
                    }
                ));
            }
        }
        if let Ok(pattern) = element
            .GetCurrentPattern(UIA_SelectionItemPatternId)
            .and_then(|unknown| Ok(unknown.cast::<IUIAutomationSelectionItemPattern>()?))
        {
            if let Ok(flag) = pattern.CurrentIsSelected() {
                parts.push(format!("selected={}", flag.as_bool()));
            }
        }
        if let Ok(pattern) = element
            .GetCurrentPattern(UIA_ExpandCollapsePatternId)
            .and_then(|unknown| Ok(unknown.cast::<IUIAutomationExpandCollapsePattern>()?))
        {
            if let Ok(state) = pattern.CurrentExpandCollapseState() {
                parts.push(format!(
                    "expand={}",
                    match state.0 {
                        0 => "Collapsed",
                        1 => "Expanded",
                        2 => "PartiallyExpanded",
                        _ => "Leaf",
                    }
                ));
            }
        }
        if let Ok(pattern) = element
            .GetCurrentPattern(UIA_RangeValuePatternId)
            .and_then(|unknown| Ok(unknown.cast::<IUIAutomationRangeValuePattern>()?))
        {
            let now = pattern.CurrentValue().map(|value| value.to_string()).unwrap_or_default();
            let min = pattern.CurrentMinimum().map(|value| value.to_string()).unwrap_or_default();
            let max = pattern.CurrentMaximum().map(|value| value.to_string()).unwrap_or_default();
            parts.push(format!("range={now}/{min}..{max}"));
        }
    }

    Ok(parts.join(" "))
}

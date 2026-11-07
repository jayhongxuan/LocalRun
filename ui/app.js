// LocalRun 前端（原生 JS，无构建依赖）
// 页面：概览（硬件+推荐）/ 模型（HF 搜索）/ 运行（下载队列+引擎）/ 对话 / 设置

async function invoke(cmd, args) {
  return window.__TAURI_INTERNALS__.invoke(cmd, args || {});
}

const $ = (sel) => document.querySelector(sel);
const pages = ['overview', 'models', 'run', 'chat', 'agent', 'image', 'video', 'settings'];

let page = 'overview';
let hw = null;
let recs = [];
let settings = null;
let netStatus = null;

let searchQuery = '';
let searchResults = [];
let searching = false;
let searchError = '';
let expandedRepo = null;
let filesCache = {};

let downloads = [];
let engine = null;        // { state, version, variant, exe, msg }
let engineProgress = null; // { stage, percent, msg }
let installingEngine = false;
let localModels = [];     // 本地 GGUF 列表（含 GGUF 元数据 n_layers/n_embd）
let runtime = null;        // 运行时状态
let runtimeTimer = null;
let ctxLen = 8192;         // 上下文长度（tokens，-c 参数）
let reasoningOpt = false;  // 思考模式
let lanAccess = false;     // 局域网访问
let localIp = '';          // 本机局域网 IPv4（lan_access 展示用）
let chatMessages = [];     // 当前会话消息 [{role, content, toolNote?, toolCalls?, toolCallId?}]
let chatBusy = false;
let toolWeb = false, toolFile = false, toolCmd = false, toolPage = false, toolDesktop = false;
let currentTheme = 'dark'; // 设置页当前选择的主题

// ---------- 会话（对话列表） ----------
let sessions = loadSessions();
let currentSessionId = null;
let pendingDelSid = null;   // 待确认删除的会话 id（确认提醒用）
let attachments = [];       // 拖入对话窗口的附件 [{name,size,kind,dataUrl,text}]
let dragDepth = 0;          // 拖拽进入/离开计数（子元素冒泡用）
function loadSessions() {
  try {
    const raw = localStorage.getItem('localrun_sessions');
    if (raw) {
      const arr = JSON.parse(raw);
      if (Array.isArray(arr)) return arr;
    }
  } catch (e) { /* 忽略 */ }
  return [{ id: 's1', title: '新对话', messages: [] }];
}
function saveSessions() {
  try { localStorage.setItem('localrun_sessions', JSON.stringify(sessions)); } catch (e) { /* 忽略 */ }
}
function currentSession() {
  return sessions.find((s) => s.id === currentSessionId) || sessions[0];
}
function bindChatToSession() {
  const s = currentSession();
  chatMessages = s ? s.messages : [];
}
function newChatSession() {
  const id = 's' + Date.now();
  sessions.unshift({ id, title: '新对话', messages: [] });
  currentSessionId = id;
  bindChatToSession();
  page = 'chat';
  render();
  saveSessions();
}
function openSession(id) {
  currentSessionId = id;
  bindChatToSession();
  page = 'chat';
  render();
}
function renderChatlist() {
  const el = $('#clist');
  if (!el) return;
  el.innerHTML = sessions.map((s) => {
    if (pendingDelSid === s.id) {
      return `<div class="cl-item cl-confirm">
        <span class="cl-confirm-txt">确认删除「${esc(s.title)}」？</span>
        <span class="cl-del ok" data-action="confirm-del">删除</span>
        <span class="cl-del" data-action="cancel-del">取消</span>
      </div>`;
    }
    return `<div class="cl-item ${s.id === currentSessionId ? 'on' : ''}">
      <button class="cl-open" data-action="open-session" data-id="${escAttr(s.id)}">${esc(s.title)}</button>
      <span class="cl-del" data-action="del-session" data-id="${escAttr(s.id)}" title="删除对话">✕</span>
    </div>`;
  }).join('');
}

// 删除对话：确认后从列表移除并持久化；删除的是当前对话则自动新建
function confirmDeleteSession() {
  if (!pendingDelSid) return;
  const id = pendingDelSid;
  pendingDelSid = null;
  sessions = sessions.filter((s) => s.id !== id);
  saveSessions();
  if (currentSessionId === id) {
    const next = sessions[0];
    if (next) { currentSessionId = next.id; bindChatToSession(); }
    else newChatSession();
  }
  renderChatlist();
  render();
  toast('对话已删除');
}

function esc(s) {
  return String(s ?? '').replace(/[&<>"']/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c]));
}
function fmtNum(n) {
  if (n >= 1e6) return (n / 1e6).toFixed(1) + 'M';
  if (n >= 1e3) return (n / 1e3).toFixed(1) + 'k';
  return String(n);
}
function fmtSize(b) {
  if (!b) return '—';
  if (b >= 1e9) return (b / 1e9).toFixed(2) + ' GB';
  if (b >= 1e6) return (b / 1e6).toFixed(1) + ' MB';
  return Math.round(b / 1024) + ' KB';
}
function fmtSpeed(bps) {
  if (!bps) return '—';
  if (bps >= 1e6) return (bps / 1e6).toFixed(1) + ' MB/s';
  if (bps >= 1e3) return (bps / 1e3).toFixed(0) + ' KB/s';
  return bps + ' B/s';
}
function shortBase(u) { return (u || '').replace(/^https?:\/\//, ''); }
function statusText(s) {
  return { queued: '排队中', downloading: '下载中', paused: '已暂停', completed: '已完成', error: '出错', cancelled: '已取消' }[s] || s;
}

function renderNav() {
  document.querySelectorAll('.nav-item').forEach((b) => {
    b.classList.toggle('on', b.dataset.page === page);
  });
}

function render() {
  renderNav();
  renderChatlist();
  const main = $('#main');
  try {
    if (page === 'overview') main.innerHTML = overviewHtml();
    else if (page === 'models') main.innerHTML = modelsHtml();
    else if (page === 'run') main.innerHTML = runHtml();
    else if (page === 'chat') main.innerHTML = chatHtml();
    else if (page === 'agent') main.innerHTML = chatHtml();
    else if (page === 'image') main.innerHTML = mediaImageHtml();
    else if (page === 'video') main.innerHTML = mediaVideoHtml();
    else main.innerHTML = settingsHtml();
  } catch (e) {
    main.innerHTML = `<div class="placeholder warn">渲染错误：${esc(String((e && e.message) || e))}</div>`;
  }
}

// ---------- 概览 ----------

function overviewHtml() {
  if (!hw) return `<div class="page"><h1>概览</h1><div class="placeholder">正在检测硬件…</div></div>`;
  const g = hw.gpu;
  const gpuCell = g.present
    ? `<div class="cell"><div class="k">显卡</div><div class="v">${esc(g.name)}</div><div class="sub">${g.unified_memory ? 'Metal 可用统一内存估算 ' + fmtSize(g.vram_total_mb * 1024 * 1024) : '显存 ' + g.vram_total_mb + ' MB · 已用 ' + g.vram_used_mb + ' MB'} · ${esc(g.driver)}</div></div>`
    : `<div class="cell"><div class="k">显卡</div><div class="v warn">未检测到可用 GPU</div><div class="sub">可使用 CPU 推理；Apple Silicon 将识别 Metal 统一内存</div></div>`;
  const recHtml = recs.length
    ? `<div class="rec-list">${recs.map((r) => `
      <div class="rec-item" data-action="rec-search" data-q="${esc(r.name)}" title="点击去模型页搜索 ${esc(r.name)}">
        <div><b>${esc(r.name)}</b> <span class="chip">${esc(r.quant)}</span></div>
        <div class="sub">${esc(r.tier_label)} · 显存 ${r.vram_gb}GB · 内存 ${r.ram_gb}GB · 体积 ${r.size_gb}GB</div>
        <div class="sub muted">${esc(r.note)}</div>
      </div>`).join('')}</div>`
    : `<div class="placeholder">无匹配模型（磁盘或内存不足）</div>`;
  return `<div class="page">
    <h1>概览</h1>
    <div class="card"><h2>本机硬件</h2><div class="grid">
      ${gpuCell}
      <div class="cell"><div class="k">处理器</div><div class="v">${esc(hw.cpu_name)}</div><div class="sub">${hw.cores} 核 ${hw.threads} 线程</div></div>
      <div class="cell"><div class="k">内存</div><div class="v">${hw.ram_total_gb} GB</div><div class="sub">可用 ${hw.ram_available_gb} GB</div></div>
      <div class="cell"><div class="k">磁盘</div><div class="v">${hw.disk_free_gb} GB 可用</div><div class="sub">共 ${hw.disk_total_gb} GB</div></div>
    </div></div>
    <div class="card"><h2>推荐模型（按本机硬件）</h2>${recHtml}</div>
    <div class="card"><h2>下一步</h2><p class="muted" style="line-height:1.8">点推荐模型直接去搜索；模型页可下载 GGUF，运行页看下载队列；推理引擎与对话在后续里程碑接入。</p></div>
  </div>`;
}

// ---------- 模型搜索 ----------

function modelsHtml() {
  const base = netStatus ? shortBase(netStatus.base) : (settings ? shortBase(settings.mirror) : '…');
  let listHtml;
  if (searching) listHtml = `<div class="placeholder">搜索中…</div>`;
  else if (searchError) listHtml = `<div class="placeholder warn">${esc(searchError)}</div>`;
  else if (searchQuery && !searchResults.length) listHtml = `<div class="placeholder">未找到匹配的 GGUF 模型，换个关键词试试</div>`;
  else if (searchResults.length) listHtml = resultsHtml();
  else listHtml = `<div class="placeholder">输入关键词，搜索 HuggingFace 上的 GGUF 模型<br><span class="muted" style="font-size:12px">例如：qwen / llama / deepseek / glm / phi</span></div>`;
  return `<div class="page">
    <h1>模型搜索</h1>
    <div class="card">
      <div class="searchbar">
        <input id="mq" placeholder="搜索 HuggingFace（GGUF）" value="${esc(searchQuery)}">
        <button class="btn primary" data-action="search">搜索</button>
      </div>
      <div class="muted sub" style="margin-top:8px">基址：${esc(base)}${netStatus ? (netStatus.ok ? ' <span class="ok">✓ 连通</span>' : ' <span class="warn">✗ 连接失败，去设置切换镜像</span>') : ''}</div>
    </div>
    ${listHtml}
  </div>`;
}

function resultsHtml() {
  return `<div class="res-list">${searchResults.map((m) => `
    <div class="res-item">
      <div class="res-head">
        <b>${esc(m.id)}</b>
        ${m.note ? `<span class="chip warn">${esc(m.note)}</span>` : ''}
        <span class="spacer"></span>
        <span class="muted sub">⬇ ${fmtNum(m.downloads)} · ♥ ${fmtNum(m.likes)} · ${esc(m.last_modified)}</span>
      </div>
      <div class="res-actions">
        <button class="btn mini" data-action="files" data-repo="${esc(m.id)}">${expandedRepo === m.id ? '收起文件' : 'GGUF 文件'}</button>
      </div>
      ${expandedRepo === m.id ? filesHtml(m.id) : ''}
    </div>`).join('')}</div>`;
}

function filesHtml(repo) {
  const f = filesCache[repo];
  if (f === undefined) return `<div class="muted sub" style="margin-top:8px">加载文件列表…</div>`;
  if (typeof f === 'string') return `<div class="warn sub" style="margin-top:8px">${esc(f)}</div>`;
  if (!f.length) return `<div class="muted sub" style="margin-top:8px">该仓库没有 GGUF 文件</div>`;
  return `<div class="file-list">${f.map((x) => `
    <div class="file-row">
      <span class="mono">${esc(x.path)}</span>
      <span class="muted">${fmtSize(x.size_bytes)}</span>
      <span class="spacer"></span>
      <button class="btn mini primary" data-action="dl" data-repo="${esc(repo)}" data-file="${esc(x.path)}" data-size="${x.size_bytes}">下载</button>
    </div>`).join('')}</div>`;
}

// ---------- 运行：下载队列 ----------

function runHtml() {
  const active = downloads.filter((d) => d.status === 'queued' || d.status === 'downloading' || d.status === 'paused' || d.status === 'error');
  return `<div class="page">
    <h1>运行</h1>
    <div class="card"><h2>下载队列（${active.length} 个进行中）</h2>${downloadsHtml()}</div>
    <div class="card"><h2>推理引擎</h2>${engineHtml()}</div>
    <div class="card"><h2>本地模型启动</h2>${modelRunHtml()}</div>
  </div>`;
}

function modelRunHtml() {
  const st = runtime || { running: false, ready: false, message: '未启动', log: [], port: 8686 };
  const stateLine = st.running
    ? `<span class="chip ${st.ready ? 'ok' : 'busy'}">${st.ready ? '运行中' : '加载中…'}</span> ${st.pid ? 'PID ' + st.pid : ''} · 端口 ${st.port}`
    : '<span class="chip">未启动</span>';
  const opts = localModels.length
    ? localModels.map((m, i) => `<option value="${escAttr(m.path)}" ${i === 0 ? 'selected' : ''}>${esc(m.name)}（${m.size_mb} MB）</option>`).join('')
    : '<option value="">未发现本地 GGUF 模型</option>';
  const log = (st.log || []).slice(-15).map((l) => esc(l)).join('\n');
  // 当前选中模型（select 第一项）的 KV 预估
  const curModel = selectedLocalModel();
  const est = kvEstimate(curModel.n_layers, curModel.n_embd, ctxLen, curModel.size_mb);
  const ramTotal = hw && hw.ram_total_gb ? hw.ram_total_gb * 1024 : 15360;
  const pct = Math.min(100, Math.round((est.totalMb / ramTotal) * 100));
  const nativeHint = curModel.context_len > 0
    ? ` · 模型原生支持 ${curModel.context_len >= 1024 ? Math.round(curModel.context_len / 1024) + 'K' : curModel.context_len} 上下文`
    : '';
  const preset = (v) => `<button class="btn mini ${ctxLen === v ? 'primary' : ''}" data-action="ctx-set" data-ctx="${v}">${v >= 1024 ? v / 1024 + 'K' : v}</button>`;
  return `<div class="dl-item" style="display:block">
    <div class="dl-head"><b>选择模型</b><button class="btn mini" data-action="refresh-models">刷新列表</button><button class="btn mini" data-action="import-model" title="选择本机任意位置的 .gguf 文件">导入本地 GGUF</button></div>
    <div class="dl-actions" style="margin:10px 0">
      <select data-action="model-select" style="min-width:340px">${opts}</select>
      <button class="btn primary" data-action="start-model">启动</button>
      <button class="btn" data-action="stop-model">停止</button>
    </div>
    <div class="ctx-box" style="margin:12px 0 4px;padding:12px;border:1px solid var(--line);border-radius:10px;background:var(--sunk)">
      <div class="dl-head"><b>上下文长度</b><span class="chip" id="ctx-val">${ctxLen} tokens</span></div>
      <div class="dl-actions" style="margin:8px 0;gap:6px">
        ${[2048, 4096, 8192, 16384, 32768, 65536, 131072].map(preset).join('')}
      </div>
      <input type="range" id="ctx-range" min="512" max="131072" step="512" value="${ctxLen}" style="width:100%">
      <div style="margin-top:10px">
        <div class="bar"><div class="bar-fill" id="kv-bar" style="width:${pct}%"></div></div>
        <div class="muted sub" id="kv-text" style="margin-top:6px">KV 缓存 ≈ ${fmtSizeMb(est.kvMb)} · 模型 ≈ ${fmtSizeMb(curModel.size_mb)} · 合计 ≈ ${fmtSizeMb(est.totalMb)}（占内存 ${pct}%）</div>
        <div class="muted sub">上下文越长，KV 缓存占用内存越大（llama-server -c 参数，下次启动生效）${nativeHint}</div>
      </div>
      <div class="dl-actions" style="margin:10px 0 0;gap:14px">
        <label class="chk"><input type="checkbox" id="opt-reasoning" ${reasoningOpt ? 'checked' : ''}> 思考模式（先思考再答，更准更慢）</label>
        <label class="chk"><input type="checkbox" id="opt-lan" ${lanAccess ? 'checked' : ''}> 局域网访问（开放 API）</label>
      </div>
      <div class="muted sub" id="lan-hint" style="margin-top:6px">${lanAccess ? 'API 地址：http://' + (localIp || '本机IP') + ':' + (settings && settings.port || 8686) + '/v1' : '开启后手机/其他电脑可通过局域网调用本地 API'}</div>
    </div>
    <div class="dl-sub muted sub">状态：${stateLine} · ${esc(st.message)}</div>
    <pre class="run-log" style="white-space:pre-wrap">${log || '（暂无日志）'}</pre>
    <div id="debug-out" class="muted sub" style="color:#f90;margin-top:6px">${window.__dbg || ''}</div>
  </div>`;
}

function selectedLocalModel() {
  const sel = document.querySelector('select[data-action="model-select"]');
  return localModels.find((m) => sel && m.path === sel.value)
    || localModels[0]
    || { n_layers: 0, n_embd: 0, size_mb: 0, context_len: 0 };
}

// KV 缓存估算：2(K+V) × 2 字节 × 层数 × 隐藏维度 × 上下文长度
function kvEstimate(layers, embd, ctx, modelMb) {
  const kvMb = layers > 0 && embd > 0 ? (2 * 2 * layers * embd * ctx) / (1024 * 1024) : 0;
  return { kvMb, totalMb: kvMb + (modelMb || 0) };
}
function fmtSizeMb(mb) {
  if (!mb) return '—';
  if (mb >= 1024) return (mb / 1024).toFixed(1) + ' GB';
  return Math.round(mb) + ' MB';
}
// 更新上下文 UI（不重建 DOM，供滑杆/档位实时刷新）
function updateCtxUi() {
  const range = document.getElementById('ctx-range');
  if (range) range.value = ctxLen;
  const val = document.getElementById('ctx-val');
  if (val) val.textContent = ctxLen + ' tokens';
  document.querySelectorAll('[data-action="ctx-set"]').forEach((b) => {
    b.classList.toggle('primary', parseInt(b.dataset.ctx, 10) === ctxLen);
  });
  const curModel = selectedLocalModel();
  const est = kvEstimate(curModel.n_layers, curModel.n_embd, ctxLen, curModel.size_mb);
  const ramTotal = hw && hw.ram_total_gb ? hw.ram_total_gb * 1024 : 15360;
  const pct = Math.min(100, Math.round((est.totalMb / ramTotal) * 100));
  const bar = document.getElementById('kv-bar');
  if (bar) bar.style.width = pct + '%';
  const txt = document.getElementById('kv-text');
  if (txt) txt.textContent = `KV 缓存 ≈ ${fmtSizeMb(est.kvMb)} · 模型 ≈ ${fmtSizeMb(curModel.size_mb)} · 合计 ≈ ${fmtSizeMb(est.totalMb)}（占内存 ${pct}%）`;
  const hint = document.getElementById('lan-hint');
  if (hint) {
    hint.textContent = lanAccess
      ? 'API 地址：http://' + (localIp || '本机IP') + ':' + ((settings && settings.port) || 8686) + '/v1'
      : '开启后手机/其他电脑可通过局域网调用本地 API';
  }
}

function engineHtml() {
  if (installingEngine) {
    const p = engineProgress || { percent: 0, msg: '…' };
    return `<div class="placeholder" style="text-align:left">
      <div class="muted" style="margin-bottom:8px">${esc(p.msg)}</div>
      <div class="bar"><div class="bar-fill" style="width:${Math.min(100, p.percent)}%"></div></div>
      <div class="muted sub" style="margin-top:8px">安装中…（后台下载 llama.cpp，可切到其他页面）</div>
    </div>`;
  }
  if (engine && engine.state === 'installed') {
    return `<div class="dl-item">
      <div class="dl-head"><b>llama.cpp ${esc(engine.version)}</b><span class="chip ok">已安装</span><span class="chip">${esc(engine.variant)}</span></div>
      <div class="dl-sub muted sub">${esc(engine.exe)}</div>
      <div class="dl-actions">
        <button class="btn mini primary" data-action="install-engine">重装/升级</button>
      </div>
    </div>`;
  }
  if (engine && engine.state === 'error') {
    return `<div class="placeholder warn" style="text-align:left">
      <div>引擎安装出错：${esc(engine.msg)}</div>
      <button class="btn primary" style="margin-top:10px" data-action="install-engine">重新安装</button>
    </div>`;
  }
  const variant = hw && hw.gpu && hw.gpu.unified_memory ? 'macOS Metal（Apple Silicon）' : (hw && hw.gpu && hw.gpu.present ? 'cuda-12.4（本机 NVIDIA）' : 'cpu（通用）');
  return `<div class="placeholder" style="text-align:left">
    <div>未安装。将自动下载 llama.cpp 官方 ${esc(variant)} 版本（GitHub Releases，走系统代理）。</div>
    <div class="muted sub" style="margin:10px 0">网络不好时可手动去 GitHub 下载 zip 后导入（后续支持）。</div>
    <button class="btn primary" data-action="install-engine">一键安装推理引擎</button>
  </div>`;
}

function downloadsHtml() {
  if (!downloads.length) return `<div class="placeholder">还没有下载任务，去模型页搜一个 GGUF 点下载</div>`;
  return `<div class="dl-list">${downloads.map((d) => {
    const pct = d.total > 0 ? Math.min(100, Math.round((d.downloaded / d.total) * 100)) : 0;
    const isDone = d.status === 'completed' || d.status === 'cancelled';
    const rowActions = d.status === 'downloading'
      ? `<button class="btn mini" data-action="pdl" data-id="${esc(d.id)}">暂停</button>`
      : d.status === 'paused'
        ? `<button class="btn mini primary" data-action="rdl" data-id="${esc(d.id)}">继续</button>`
        : d.status === 'error'
          ? `<button class="btn mini primary" data-action="rdl" data-id="${esc(d.id)}">重试</button>`
          : '';
    return `<div class="dl-item">
      <div class="dl-head">
        <span class="mono" title="${esc(d.repo)}">${esc(d.file)}</span>
        <span class="chip ${d.status === 'error' ? 'warn' : ''}">${statusText(d.status)}</span>
        <span class="spacer"></span>
        <span class="muted sub">${fmtSize(d.downloaded)} / ${fmtSize(d.total)} · ${fmtSpeed(d.speed_bps)}</span>
      </div>
      <div class="bar"><div class="bar-fill" style="width:${pct}%"></div></div>
      <div class="dl-sub muted sub">${d.repo} · ${pct}%${d.error ? ' · <span class="warn">' + esc(d.error) + '</span>' : ''}</div>
      <div class="dl-actions">
        ${rowActions}
        ${isDone ? '' : `<button class="btn mini" data-action="cdl" data-id="${esc(d.id)}">取消</button>`}
      </div>
    </div>`;
  }).join('')}</div>`;
}

// ---------- 对话 / 设置 ----------

function chatHtml() {
  const agentMode = page === 'agent';
  const st = runtime || {};
  const stLine = st.running
    ? `<span class="chip ${st.ready ? 'ok' : 'busy'}">${st.ready ? '运行中' : '加载中…'}</span> <span class="muted sub">PID ${st.pid || '—'} · 端口 ${st.port || '—'}</span>`
    : '<span class="chip warn">未启动</span> <span class="muted sub">请先到「运行」页启动模型</span>';
  const bubbles = chatMessages.map((m) => {
    const cls = m.role === 'user' ? 'user' : (m.role === 'assistant' ? 'assistant' : 'tool');
    const note = m.toolNote ? `<div class="tool-note">🔧 ${esc(m.toolNote)}</div>` : '';
    const imageHtml = Array.isArray(m.images) && m.images.length
      ? `<div class="msg-images">${m.images.map((img) => `<img class="msg-image" src="${escAttr(img.dataUrl || '')}" alt="${escAttr(img.name || '图片')}" title="${escAttr(img.path || '')}">`).join('')}</div>`
      : '';
    return `<div class="msg ${cls}">${messageContentHtml(m.content)}${imageHtml}${note}</div>`;
  }).join('');
  return `<div class="page chat-page">
    <h1>${agentMode ? 'Agent' : '对话'} <span class="muted sub" style="font-weight:400">${stLine}</span></h1>
    ${agentMode ? '<div class="muted sub" style="margin:-6px 0 8px">本地 Agent 使用当前 llama.cpp 模型；本地文件工具可离线工作，联网工具仅在你勾选后访问网络。执行命令每次都会弹出系统确认框。</div>' : ''}
    <div class="chat-log" id="chat-log">${bubbles || '<div class="placeholder">模型未运行时输入也可，但会在发送时提示先启动模型。</div>'}</div>
    ${attachmentsHtml()}
    <div class="chat-input">
      <textarea id="chat-input" rows="3" placeholder="输入消息…（Enter 发送，Shift+Enter 换行）"></textarea>
      <div class="chat-tools">
        <button class="btn mini" data-action="pick-attach" title="选择附件（也可从文件管理器直接拖拽进来）">📎</button>
        <label class="chk"><input type="checkbox" id="tool-web" ${toolWeb ? 'checked' : ''}> 联网搜索</label>
        <label class="chk"><input type="checkbox" id="tool-file" ${toolFile ? 'checked' : ''}> 读文件</label>
        <label class="chk"><input type="checkbox" id="tool-cmd" ${toolCmd ? 'checked' : ''}> 执行命令（系统弹窗逐次确认）</label>
        <label class="chk"><input type="checkbox" id="tool-page" ${toolPage ? 'checked' : ''}> 读网页</label>
        <label class="chk"><input type="checkbox" id="tool-desktop" ${toolDesktop ? 'checked' : ''}> 桌面图片</label>
        <span class="spacer"></span>
        <button class="btn mini" data-action="clear-chat">清空</button>
        <button class="btn primary" data-action="send-chat" ${chatBusy ? 'disabled' : ''}>${chatBusy ? '生成中…' : '发送'}</button>
      </div>
    </div>
  </div>`;
}

function messageContentHtml(content) {
  if (!Array.isArray(content)) return esc(content);
  return content.map((part) => {
    if (part && part.type === 'text') return esc(part.text || '');
    if (part && part.type === 'image_url') {
      const url = part.image_url && part.image_url.url;
      if (typeof url === 'string' && /^data:image\/(png|jpeg|webp|gif|bmp);base64,/i.test(url)) {
        return `<img class="msg-image" src="${escAttr(url)}" alt="图片附件">`;
      }
      return '<span class="muted">[图片附件]</span>';
    }
    return '';
  }).join('');
}

function messagePlainText(content) {
  if (typeof content === 'string') return content;
  if (Array.isArray(content)) return content.filter((p) => p && p.type === 'text').map((p) => p.text || '').join(' ');
  return '';
}

function buildToolDefs() {
  const defs = [];
  if (toolWeb) defs.push({ type: 'function', function: { name: 'web_search', description: '联网搜索获取最新信息，返回搜索结果列表（标题/来源/摘要/链接）', parameters: { type: 'object', properties: { query: { type: 'string', description: '搜索关键词' } }, required: ['query'] } } });
  if (toolFile) defs.push({ type: 'function', function: { name: 'read_file', description: '读取本地文本文件内容，返回前若干行', parameters: { type: 'object', properties: { path: { type: 'string', description: '文件绝对路径' }, max_lines: { type: 'integer', description: '最多读取行数' } }, required: ['path'] } } });
  if (toolCmd) defs.push({ type: 'function', function: { name: 'run_command', description: '在本机执行 shell 命令；仅在用户明确要求时调用，用户会在每次执行前确认', parameters: { type: 'object', properties: { command: { type: 'string', description: '要执行的本机命令' } }, required: ['command'] } } });
  if (toolPage) defs.push({ type: 'function', function: { name: 'fetch_web', description: '读取指定网页的内容并提取正文文本，用于打开链接/查看网页/获取页面信息', parameters: { type: 'object', properties: { url: { type: 'string', description: '网页完整链接（http/https）' } }, required: ['url'] } } });
  if (toolDesktop) defs.push({ type: 'function', function: { name: 'list_desktop_images', description: '列出用户桌面上的截图/图片（png/jpg/jpeg/webp/gif/bmp），把缩略图显示在聊天中。用户说“调取桌面截图”“看看桌面图片”“图片呢”时必须调用本工具，不要编造文件名或普通文本工具调用。', parameters: { type: 'object', properties: { limit: { type: 'integer', description: '最多显示几张，1-12，默认 6' } } } } });
  return defs;
}

async function invokeAgentTool(call) {
  const name = call.function.name;
  const rawArgs = call.function.arguments || '{}';
  // run_command 的逐次确认由 Rust 端原生对话框强制执行；
  // 这样即使前端被绕过、直接 invoke execute_tool，也无法跳过确认。
  return invoke('execute_tool', { name, argsJson: rawArgs });
}

// ---------- 附件（拖拽图片/文件进对话窗口） ----------
function showDrop() {
  const o = $('#dropOverlay');
  if (o) o.hidden = false;
}
function hideDrop() {
  const o = $('#dropOverlay');
  if (o) o.hidden = true;
}
// HTML5 兜底（部分环境 Tauri 不拦截 DOM drop 时可用）
document.addEventListener('dragenter', (e) => { e.preventDefault(); dragDepth++; showDrop(); });
document.addEventListener('dragover', (e) => { e.preventDefault(); });
document.addEventListener('dragleave', (e) => { if (--dragDepth <= 0) { dragDepth = 0; hideDrop(); } });
document.addEventListener('drop', async (e) => {
  e.preventDefault();
  dragDepth = 0;
  hideDrop();
  const files = e.dataTransfer && e.dataTransfer.files;
  if (!files || !files.length) return;
  for (const f of files) await addAttachment(f);
  render();
  toast('已添加 ' + files.length + ' 个附件');
});
// Tauri 拖放事件（Windows 主路径：拖入文件 → 绝对路径 → Rust 读附件）
try {
  const internals = window.__TAURI_INTERNALS__;
  if (internals && internals.invoke && internals.transformCallback) {
    const tListen = (event, handler) => internals.invoke('plugin:event|listen', {
      event,
      target: { kind: 'Any' },
      handler: internals.transformCallback(handler),
    });
    tListen('tauri://drag-enter', () => { dragDepth++; showDrop(); });
    tListen('tauri://drag-over', () => showDrop());
    tListen('tauri://drag-leave', () => { dragDepth = 0; hideDrop(); });
    tListen('tauri://drag-drop', async (e) => {
      dragDepth = 0;
      hideDrop();
      const paths = (e && e.payload && e.payload.paths) || [];
      let added = 0;
      for (const p of paths) {
        try {
          const a = await invoke('read_attachment', { path: p });
          if (a) { attachments.push({ name: a.name, size: a.size, kind: a.kind, dataUrl: a.data_url || '', text: a.text || '' }); added++; }
        } catch (err) { toast('读取附件失败：' + err, true); }
      }
      if (added) { render(); toast('已添加 ' + added + ' 个附件'); }
    });
  }
} catch (e) { /* 忽略：HTML5 兜底继续 */ }
function readFileAsDataURL(f) {
  return new Promise((res, rej) => { const r = new FileReader(); r.onload = () => res(r.result); r.onerror = () => rej(r.error); r.readAsDataURL(f); });
}
function readFileAsText(f) {
  return new Promise((res, rej) => { const r = new FileReader(); r.onload = () => res(r.result); r.onerror = () => rej(r.error); r.readAsText(f); });
}
async function addAttachment(f) {
  const name = f.name || '文件';
  const size = f.size || 0;
  const kind = f.type && f.type.startsWith('image/') ? 'image'
    : (f.type && f.type.startsWith('text/')) || /\.(txt|md|json|csv|log|py|rs|js|ts|html|css|xml|yaml|yml|toml|ini|sh|bat|ps1)$/i.test(name) ? 'text'
    : 'file';
  const a = { name, size, kind, dataUrl: '', text: '' };
  try {
    if (kind === 'image') a.dataUrl = await readFileAsDataURL(f);
    else if (kind === 'text') a.text = await readFileAsText(f);
  } catch (err) { /* 读取失败也保留条目（仅文件名） */ }
  attachments.push(a);
}
function attachmentsHtml() {
  if (!attachments.length) return '';
  return `<div class="attach-bar">${attachments.map((a, i) => `
    <div class="attach-chip">
      ${a.kind === 'image' && a.dataUrl ? `<img class="attach-thumb" src="${a.dataUrl}" alt="">` : '<span class="attach-ico">📄</span>'}
      <span class="attach-name" title="${esc(a.name)}">${esc(a.name)}</span>
      <span class="muted sub">${fmtSize(a.size)}</span>
      <span class="attach-del" data-action="remove-attach" data-i="${i}">✕</span>
    </div>`).join('')}</div>`;
}

const AGENT_SYSTEM_PROMPT = '你是运行在用户本机的 LocalRun AI 助手，推理由当前本地模型完成。工具仅限本轮请求中提供的工具。联网搜索/网页读取只在用户启用相应工具时使用。执行本机命令属于高风险操作：只有用户明确要求时才提出调用，并等待应用原生弹窗中的逐次确认；不要声称命令已执行，除非工具返回成功结果。读取本地文件时只读用户指定或与任务直接相关的文件。当用户要求查看、调取或显示桌面截图/图片时：如果工具表中有 list_desktop_images，必须调用它，不要用普通文本伪造工具调用；如果没有该工具，明确请用户勾选「桌面图片」，不要编造 run.json 或读取文件。该工具只把图片显示给用户，本地模型没有视觉能力，不要声称看懂了图片像素内容。';

function chatApiMessages() {
  const list = [{ role: 'system', content: AGENT_SYSTEM_PROMPT }];
  for (const m of chatMessages) {
    const o = { role: m.role, content: m.content };
    if (m.toolCalls && m.toolCalls.length) o.tool_calls = m.toolCalls;
    if (m.toolCallId) o.tool_call_id = m.toolCallId;
    list.push(o);
  }
  return list;
}

// 真正的 Agent 循环：模型请求 → 工具执行 → tool 结果回填 → 再请求模型。
async function runChatLoop(tools) {
  const MAX_TOOL_ROUNDS = 8;
  let toolRounds = 0;
  while (toolRounds < MAX_TOOL_ROUNDS) {
    const resp = await invoke('chat_completion', { req: { messages: chatApiMessages(), tools: tools.length ? tools : null } });
    const j = JSON.parse(resp);
    const msg = j.choices && j.choices[0] && j.choices[0].message;
    const calls = (msg && msg.tool_calls) || [];
    if (!calls.length) {
      chatMessages.push({ role: 'assistant', content: (msg && msg.content) || '（模型返回了空内容）' });
      return;
    }
    toolRounds++;
    chatMessages.push({
      role: 'assistant',
      content: msg.content || '',
      toolNote: `🔧 调用工具：${calls.map((t) => t.function.name).join(', ')}（第 ${toolRounds}/${MAX_TOOL_ROUNDS} 轮）`,
      toolCalls: calls,
    });
    for (const c of calls) {
      let result = '';
      let resultImages = [];
      try {
        const raw = await invokeAgentTool(c);
        if (raw && typeof raw === 'object' && !Array.isArray(raw)) {
          result = String(raw.content ?? '');
          resultImages = Array.isArray(raw.images) ? raw.images : [];
        } else {
          result = String(raw ?? '');
        }
      } catch (e) {
        result = '工具执行出错：' + e;
      }
      chatMessages.push({ role: 'tool', content: result, toolCallId: c.id, toolNote: `${c.function.name} 结果`, images: resultImages });
    }
    render();
  }
  chatMessages.push({ role: 'assistant', content: '', toolNote: `连续工具调用达到 ${MAX_TOOL_ROUNDS} 轮上限，已自动暂停。` });
}

async function sendChat() {
  const inp = $('#chat-input');
  const text = (inp.value || '').trim();
  if ((!text && !attachments.length) || chatBusy) return;
  // 没有启用桌面图片工具时，直接提示，避免模型编造 run.json/read_file 之类的伪工具调用。
  if (!toolDesktop && !attachments.length && /桌面/.test(text) && /(截图|图片|截屏)/.test(text)) {
    toast('请先勾选工具栏里的「桌面图片」，再发送这条请求', true);
    return;
  }
  // 合成用户消息：文本 + 附件（图片走多模态 image_url，文本文件读入上下文，其他文件标注）
  const parts = [];
  if (text) parts.push({ type: 'text', text });
  for (const a of attachments) {
    if (a.kind === 'image' && a.dataUrl) parts.push({ type: 'image_url', image_url: { url: a.dataUrl } });
    else if (a.kind === 'text' && a.text) parts.push({ type: 'text', text: `\n\n[附加文件：${a.name}]\n${a.text.slice(0, 12000)}` });
    else parts.push({ type: 'text', text: `\n\n[已附加文件：${a.name}（${fmtSize(a.size)}），如需内容请让我用 read_file 读取]` });
  }
  const userMsg = { role: 'user', content: parts.length ? parts : text };
  const sentAttachments = attachments;
  chatMessages.push(userMsg);
  attachments = [];
  inp.value = '';
  chatBusy = true;
  render();
  const tools = buildToolDefs();
  try {
    await runChatLoop(tools);
  } catch (e) {
    const errText = String(e);
    const imageAttachments = sentAttachments.filter((a) => a.kind === 'image');
    if (imageAttachments.length && errText.includes('VISION_UNSUPPORTED')) {
      // 所选模型没有视觉能力：明确告知用户，并把图片替换为文件名后完整重跑工具循环。
      let imageIndex = 0;
      const fallbackParts = parts.map((part) => {
        if (part.type !== 'image_url') return part;
        const name = (imageAttachments[imageIndex++] || {}).name || '图片';
        return { type: 'text', text: `\n\n[已附加图片：${name}]（当前模型未配置视觉输入，仅发送文件名）` };
      });
      userMsg.content = fallbackParts.length ? fallbackParts : (text || '');
      // 只显示在用户气泡里，不写入 API 历史；否则会在 user 后插入 assistant，
      // 导致下一次请求的对话顺序错误。
      userMsg.toolNote = '⚠️ 当前本地模型未配置视觉投影文件（mmproj），已移除图片并仅以文件名重试。';
      render();
      try {
        await runChatLoop(tools);
      } catch (e2) {
        chatMessages.push({ role: 'assistant', content: '⚠️ ' + String(e2) });
      }
    } else {
      chatMessages.push({ role: 'assistant', content: '⚠️ ' + errText });
    }
  }
  chatBusy = false;
  render();
  const log2 = $('#chat-log');
  if (log2) log2.scrollTop = log2.scrollHeight;
  // 会话标题取第一条用户消息
  const s = currentSession();
  if (s) {
    const firstUser = chatMessages.find((m) => m.role === 'user');
    if (firstUser) {
      const t = messagePlainText(firstUser.content).replace(/\s+/g, ' ').slice(0, 18);
      if (s.title === '新对话' && t) s.title = t + '…';
    }
    saveSessions();
  }
}

function settingsHtml() {
  const s = settings || {};
  const presets = ['https://huggingface.co', 'https://hf-mirror.com'];
  const cur = (s.mirror || '').replace(/\/$/, '');
  const isPreset = presets.includes(cur);
  return `<div class="page">
    <h1>设置</h1>
    <div class="card"><h2>模型与网络</h2>
      <div class="form-row"><label>模型下载目录</label><input id="set-dir" value="${esc(s.models_dir || '')}" placeholder="~/Models"></div>
      <div class="form-row"><label>连接基址（搜索与下载都走这里）</label>
        <select id="set-mirror">
          ${presets.map((p) => `<option value="${p}" ${p === cur ? 'selected' : ''}>${p === 'https://huggingface.co' ? '直连官网 huggingface.co' : '镜像 hf-mirror.com'}</option>`).join('')}
          <option value="__custom" ${isPreset ? '' : 'selected'}>自定义…</option>
        </select>
        <input id="set-mirror-custom" placeholder="https://huggingface.co" value="${isPreset ? '' : esc(cur)}" style="${isPreset ? 'display:none' : ''}">
        <div class="muted sub">默认直连 HuggingFace；连不上时切 hf-mirror.com 镜像</div>
      </div>
      <div class="form-row"><label>推理端口（后续 llama-server 用）</label><input id="set-port" type="number" min="1024" max="65535" value="${s.port || 8686}"></div>
      <div class="form-row"><label>并行下载数（1–8）</label><input id="set-par" type="number" min="1" max="8" value="${s.parallel_downloads || 2}"></div>
      <div class="form-row"><label>界面主题</label>
        <div class="theme-row">
          ${['dark', 'light', 'sepia'].map((t) => {
            const names = { dark: '深色', light: '明亮', sepia: '淡黄' };
            const on = (s.theme || 'dark') === t;
            return `<button class="btn mini ${on ? 'primary' : ''}" data-action="pick-theme" data-theme="${t}">${names[t]}</button>`;
          }).join('')}
        </div>
        <div class="muted sub">点击即切换，点「保存设置」持久化</div>
      </div>
      <div class="form-row"><button class="btn primary" data-action="save-settings">保存设置</button> <span id="save-hint"></span></div>
    </div>
  </div>`;
}

// ---------- 动作 ----------

async function doSearch() {
  const q = ($('#mq').value || '').trim();
  if (!q || searching) return;
  searchQuery = q;
  searchError = '';
  searchResults = [];
  expandedRepo = null;
  searching = true;
  render();
  try {
    searchResults = await invoke('search_hf_models', { query: q });
  } catch (e) {
    searchError = String(e);
  }
  searching = false;
  render();
}

async function toggleFiles(repo) {
  if (expandedRepo === repo) {
    expandedRepo = null;
    render();
    return;
  }
  expandedRepo = repo;
  render();
  if (filesCache[repo] === undefined) {
    try {
      filesCache[repo] = await invoke('get_hf_files', { repoId: repo });
    } catch (e) {
      filesCache[repo] = '加载失败：' + e;
    }
    render();
  }
}

async function doDl(repo, file, size) {
  page = 'run';
  render();
  try {
    const id = await invoke('add_download', { repoId: repo, file, size });
    await invoke('start_download', { id });
    toast(`已加入下载：${file}`);
    await refreshDownloads();
    render();
  } catch (e) {
    toast('下载失败：' + e, true);
    render();
  }
}

async function pauseDl(id) { try { await invoke('pause_download', { id }); } catch (e) { toast(e, true); } }
async function resumeDl(id) { try { await invoke('resume_download', { id }); } catch (e) { toast(e, true); } }
async function cancelDl(id) { try { await invoke('cancel_download', { id }); } catch (e) { toast(e, true); } }

async function refreshLocalModels() {
  try { localModels = await invoke('list_local_models'); render(); } catch (e) { toast('读取本地模型失败：' + e, true); }
}

async function startModel() {
  const sel = document.querySelector('select[data-action="model-select"]');
  if (!sel || !sel.value) { toast('请先选择模型', true); return; }
  // 启动前持久化上下文长度/思考模式/局域网访问（本次与后续启动一致）
  try {
    const ns = {
      ...(settings || {}),
      models_dir: settings?.models_dir || '',
      mirror: settings?.mirror || 'https://huggingface.co',
      port: settings?.port || 8686,
      parallel_downloads: settings?.parallel_downloads || 2,
      theme: currentTheme,
      ctx_len: ctxLen,
      reasoning: reasoningOpt,
      lan_access: lanAccess,
    };
    settings = await invoke('save_settings', { settings: ns });
  } catch (e) { /* 设置保存失败不阻塞启动 */ }
  window.__dbg = 'invoking start_model...';
  render();
  try {
    // 大模型在 Metal 上加载可能超过 40 秒；后端最多等待 300 秒。
    // 先启动状态轮询，让「加载中」状态可见，再等待最终结果。
    startRuntimePoll();
    toast('正在加载模型，可能需要数十秒到数分钟…');
    await invoke('start_model', { path: sel.value });
    runtime = await invoke('get_runtime_status');
    window.__dbg = 'OK: ' + JSON.stringify(runtime).slice(0, 160);
    render();
    toast(runtime.ready ? '模型已就绪' : (runtime.message || '模型启动失败'));
    startRuntimePoll();
  } catch (e) {
    window.__dbg = 'ERR: ' + String(e);
    render();
    toast('模型启动失败：' + e, true);
  }
}

async function stopModel() {
  try { runtime = await invoke('stop_model'); render(); toast('已停止'); } catch (e) { toast(e, true); }
}

async function pollRuntime() {
  try { runtime = await invoke('get_runtime_status'); if (page === 'run') render(); } catch (e) { /* 忽略 */ }
}

function startRuntimePoll() {
  if (runtimeTimer) clearInterval(runtimeTimer);
  runtimeTimer = setInterval(() => { pollRuntime(); }, 2000);
}

function escAttr(s) { return String(s).replace(/&/g, '&amp;').replace(/"/g, '&quot;').replace(/</g, '&lt;'); }

async function installEngine() {
  if (installingEngine) return;
  installingEngine = true;
  engineProgress = { stage: 'fetch', percent: 0, msg: '开始安装…' };
  render();
  try {
    await invoke('install_engine', { variant: 'auto' });
    engine = await invoke('get_engine_status');
    toast('推理引擎安装完成');
  } catch (e) {
    toast('引擎安装失败：' + e, true);
  }
  installingEngine = false;
  engineProgress = null;
  render();
}

async function refreshDownloads() {
  try { downloads = await invoke('get_downloads'); } catch (e) { /* 忽略 */ }
}

function toast(msg, isErr) {
  const t = document.createElement('div');
  t.className = isErr ? 'toast warn' : 'toast';
  t.textContent = msg;
  document.body.appendChild(t);
  setTimeout(() => t.remove(), 3500);
}

async function saveSettings() {
  const sel = $('#set-mirror').value;
  const mirror = sel === '__custom'
    ? ($('#set-mirror-custom').value.trim() || 'https://huggingface.co')
    : sel;
  const ns = {
    ...(settings || {}),
    models_dir: $('#set-dir').value.trim(),
    mirror,
    port: parseInt($('#set-port').value, 10) || 8686,
    parallel_downloads: parseInt($('#set-par').value, 10) || 2,
    theme: currentTheme,
    ctx_len: ctxLen,
    reasoning: reasoningOpt,
    lan_access: lanAccess,
  };
  const hint = $('#save-hint');
  hint.className = '';
  hint.textContent = '';
  try {
    settings = await invoke('save_settings', { settings: ns });
    hint.className = 'ok';
    hint.textContent = '已保存 ✓';
    checkNet();
  } catch (e) {
    hint.className = 'warn';
    hint.textContent = '保存失败：' + e;
  }
}

async function refreshHardware() {
  try {
    hw = await invoke('get_hardware_info');
  } catch (e) {
    hw = null;
    $('#statusbar').insertAdjacentHTML('beforeend', `<span class="warn">硬件检测失败：${esc(e)}</span>`);
  }
  if (hw) {
    try {
      recs = await invoke('recommend_models', { hw });
    } catch (e) {
      recs = [];
    }
  }
  render();
}

async function checkNet() {
  const el = $('#st-net');
  el.textContent = '网络检测中…';
  el.className = '';
  try {
    const n = await invoke('check_hf_connection');
    netStatus = n;
    el.textContent = `${shortBase(n.base)} ${n.ok ? '✓ ' + n.ms + 'ms' : '✗ 连接失败'}`;
    el.className = n.ok ? 'ok' : 'warn';
  } catch (e) {
    el.textContent = '网络检测失败';
  }
  updateStatusbar();
}

// 状态栏：推理引擎状态（引擎实际已装但 HTML 初始值恒为"未安装"的修复）
function updateStatusbar() {
  const el = $('#st-engine');
  if (!el) return;
  if (engine && engine.state === 'installed') {
    el.textContent = `推理引擎：已就绪${engine.version ? ' (' + engine.version + ')' : ''}`;
    el.className = 'ok';
  } else {
    el.textContent = '推理引擎：未安装';
    el.className = 'warn';
  }
}

// ---------- 事件（委托 + Tauri 事件） ----------

document.addEventListener('click', (ev) => {
  const el = ev.target.closest('[data-action]');
  if (!el) return;
  const act = el.dataset.action;
  if (act === 'search') { doSearch(); }
  else if (act === 'rec-search') {
    searchQuery = el.dataset.q;
    searchResults = [];
    searchError = '';
    page = 'models';
    render();
    doSearch();
  } else if (act === 'files') { toggleFiles(el.dataset.repo); }
  else if (act === 'dl') { doDl(el.dataset.repo, el.dataset.file, parseInt(el.dataset.size, 10) || 0); }
  else if (act === 'pdl') { pauseDl(el.dataset.id); }
  else if (act === 'rdl') { resumeDl(el.dataset.id); }
  else if (act === 'cdl') { cancelDl(el.dataset.id); }
  else if (act === 'install-engine') { installEngine(); }
  else if (act === 'refresh-models') { refreshLocalModels(); }
  else if (act === 'import-model') {
    (async () => {
      try {
        const path = await invoke('pick_gguf');
        if (!path) return;
        const m = await invoke('import_local_model', { path });
        toast('已导入：' + (m && m.name || path));
        await refreshLocalModels();
      } catch (e) {
        toast('导入失败：' + e, true);
      }
    })();
  }
  else if (act === 'start-model') { startModel(); }
  else if (act === 'del-session') { pendingDelSid = el.dataset.id || null; renderChatlist(); }
  else if (act === 'remove-attach') { const i = parseInt(el.dataset.i, 10); if (!isNaN(i) && attachments[i]) { attachments.splice(i, 1); render(); } }
  else if (act === 'pick-attach') {
    const fi = document.createElement('input');
    fi.type = 'file';
    fi.multiple = true;
    fi.style.display = 'none';
    document.body.appendChild(fi);
    fi.addEventListener('change', async () => {
      const fs = Array.from(fi.files || []);
      for (const f of fs) await addAttachment(f);
      if (fs.length) { render(); toast('已添加 ' + fs.length + ' 个附件'); }
    });
    fi.click();
  }
  else if (act === 'confirm-del') { confirmDeleteSession(); }
  else if (act === 'cancel-del') { pendingDelSid = null; renderChatlist(); }
  else if (act === 'ctx-set') { ctxLen = parseInt(el.dataset.ctx, 10) || 8192; updateCtxUi(); }
  else if (act === 'stop-model') { stopModel(); }
  else if (act === 'send-chat') { sendChat(); }
  else if (act === 'clear-chat') { chatMessages = []; saveSessions(); render(); }
  else if (act === 'new-chat') { newChatSession(); }
  else if (act === 'open-session') { openSession(el.dataset.id); }
  else if (act === 'pick-theme') {
    currentTheme = el.dataset.theme;
    applyTheme(currentTheme);
    document.querySelectorAll('[data-action="pick-theme"]').forEach((b) => {
      b.classList.toggle('primary', b.dataset.theme === currentTheme);
    });
  }
  else if (act === 'save-settings') { saveSettings(); }
});

$('#main').addEventListener('keydown', (ev) => {
  if (ev.target.id === 'mq' && ev.key === 'Enter') { ev.preventDefault(); doSearch(); }
  if (ev.target.id === 'chat-input' && ev.key === 'Enter' && !ev.shiftKey) { ev.preventDefault(); sendChat(); }
});

$('#main').addEventListener('change', (ev) => {
  if (ev.target.id === 'set-mirror') {
    const custom = $('#set-mirror-custom');
    const show = ev.target.value === '__custom';
    custom.style.display = show ? '' : 'none';
    if (show && !custom.value) custom.value = (settings?.mirror || '').replace(/\/$/, '');
  }
  if (ev.target.id === 'tool-web') toolWeb = ev.target.checked;
  if (ev.target.id === 'tool-file') toolFile = ev.target.checked;
  if (ev.target.id === 'tool-cmd') toolCmd = ev.target.checked;
  if (ev.target.id === 'tool-page') toolPage = ev.target.checked;
  if (ev.target.id === 'tool-desktop') toolDesktop = ev.target.checked;
  if (ev.target.matches('select[data-action="model-select"]')) updateCtxUi();
  if (ev.target.id === 'opt-reasoning') { reasoningOpt = ev.target.checked; updateCtxUi(); }
  if (ev.target.id === 'opt-lan') { lanAccess = ev.target.checked; updateCtxUi(); refreshLocalIp(); }
});
// 局域网 IP 兜底：勾选/进入运行页时异步拉取本机 IP 用于 API 地址展示
async function refreshLocalIp() {
  try {
    const ips = await invoke('get_local_ip');
    if (ips && ips[0]) localIp = ips[0];
  } catch (e) { /* 忽略 */ }
  updateCtxUi();
}
// 上下文滑杆：实时更新（input 事件冒泡）
document.addEventListener('input', (ev) => {
  if (ev.target && ev.target.id === 'ctx-range') {
    ctxLen = parseInt(ev.target.value, 10) || 8192;
    updateCtxUi();
  }
});

document.querySelector('.nav').addEventListener('click', (ev) => {
  const b = ev.target.closest('.nav-item');
  if (!b) return;
  page = b.dataset.page;
  render();
  if (page === 'run') refreshDownloads().then(render);
});

// Tauri 事件监听（带轮询兜底）
function handleDlPayload(payload) {
  if (Array.isArray(payload)) {
    downloads = payload;
  } else {
    const idx = downloads.findIndex((d) => d.id === payload.id);
    if (idx >= 0) downloads[idx] = payload;
    else downloads.push(payload);
  }
  if (page === 'run') render();
}

function onDlEvent(cb) {
  const w = window.__TAURI_INTERNALS__;
  if (w && w.event && typeof w.event.listen === 'function') {
    w.event.listen('download-progress', (e) => cb(e.payload)).catch(() => {});
  }
  setInterval(() => { invoke('get_downloads').then(cb).catch(() => {}); }, 1000);
}

onDlEvent(handleDlPayload);

// 引擎安装进度事件
function onEngineEvent(cb) {
  const w = window.__TAURI_INTERNALS__;
  if (w && w.event && typeof w.event.listen === 'function') {
    w.event.listen('engine-progress', (e) => cb(e.payload)).catch(() => {});
  }
}
onEngineEvent((p) => {
  if (p.stage === 'done' || p.stage === 'error') return;
  engineProgress = p;
  if (page === 'run') render();
});

// 主题切换：立即生效
function applyTheme(t) {
  const v = ['dark', 'light', 'sepia'].includes(t) ? t : 'dark';
  document.documentElement.dataset.theme = v;
}

(async () => {
  render();
  bindChatToSession();
  renderChatlist();
  try {
    settings = await invoke('get_settings');
    currentTheme = settings.theme || 'dark';
    applyTheme(currentTheme);
    ctxLen = settings.ctx_len || 8192;
    reasoningOpt = !!settings.reasoning;
    lanAccess = !!settings.lan_access;
  } catch (e) { /* 保持默认 */ }
  try { const ips = await invoke('get_local_ip'); localIp = (ips && ips[0]) || ''; updateCtxUi(); } catch (e) { /* 忽略 */ }
  checkNet();
  await refreshDownloads();
  await refreshHardware();
  try { engine = await invoke('get_engine_status'); } catch (e) { /* 忽略 */ }
  updateStatusbar();
  await refreshLocalModels();
  try { runtime = await invoke('get_runtime_status'); if (runtime.running) startRuntimePoll(); } catch (e) { /* 忽略 */ }
})();

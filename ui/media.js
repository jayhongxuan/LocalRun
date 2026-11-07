// 本地媒体生成界面：仅调用用户选择的本机 stable-diffusion.cpp sd-cli。
let mSettings = null;
let mEngine = null;
let mJob = { state: 'idle', message: '当前没有媒体任务', outputPath: null, logTail: '' };
let mLoading = false;
let mLoadPromise = null;
let mRefreshTimer = null;

function mInvoke(cmd, args) {
  return window.__TAURI_INTERNALS__.invoke(cmd, args || {});
}
function mEsc(value) {
  return String(value ?? '').replace(/[&<>"']/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c]));
}
function mPath(path) { return path ? String(path) : '尚未选择'; }
function mField(title, field, kind) {
  const path = mSettings ? mSettings[field] : '';
  return `<div class="form-row"><label>${mEsc(title)}</label><div class="media-file-row"><input readonly value="${mEsc(mPath(path))}" title="${mEsc(path || '')}"><button class="btn mini" data-media-action="pick-file" data-kind="${mEsc(kind)}">选择文件</button></div></div>`;
}
function mEngineCard() {
  const ready = mEngine && mEngine.available;
  return `<div class="card"><h2>本地扩散引擎</h2>
    <div class="media-file-row"><input readonly value="${mEsc(mPath(mSettings && mSettings.sdCliPath))}" title="${mEsc(mSettings && mSettings.sdCliPath || '')}"><button class="btn mini" data-media-action="pick-engine">选择 sd-cli</button></div>
    <div class="muted sub" style="margin-top:8px">${mEsc(mEngine ? mEngine.message : '正在检查本地引擎…')}。生成在本机完成，不调用云端生成服务。</div>
    ${!ready ? '<div class="muted sub" style="margin-top:6px">请先从 <a href="https://github.com/leejet/stable-diffusion.cpp/releases" target="_blank" rel="noreferrer">stable-diffusion.cpp 官方 Releases</a> 获取适用于本机架构的 sd-cli，再选择可执行文件。</div>' : ''}
  </div>`;
}
function mJobCard() {
  const state = mJob && mJob.state || 'idle';
  const stateLabel = ({ idle: '空闲', running: '生成中', succeeded: '已完成', failed: '失败', cancelled: '已取消' })[state] || state;
  return `<div class="card"><div class="dl-head"><h2 style="margin:0">生成任务</h2><span class="chip ${state === 'failed' ? 'warn' : state === 'succeeded' ? 'ok' : state === 'running' ? 'busy' : ''}">${mEsc(stateLabel)}</span><span class="spacer"></span>${state === 'running' && mJob.jobId ? `<button class="btn mini" data-media-action="cancel" data-job="${mEsc(mJob.jobId)}">取消任务</button>` : ''}</div>
    <div id="media-job-message" class="muted sub" style="margin-top:8px">${mEsc(mJob && mJob.message || '当前没有媒体任务')}</div>
    ${mJob && mJob.outputPath ? `<div class="muted sub" style="margin-top:5px;word-break:break-all">输出文件：${mEsc(mJob.outputPath)}</div>` : ''}
    ${state === 'succeeded' && mJob.outputPath ? `<button class="btn mini" style="margin-top:8px" data-media-action="reveal-output" data-output="${mEsc(mJob.outputPath)}">在文件管理器中显示</button>` : ''}
    ${mJob && mJob.logTail ? `<details style="margin-top:8px"><summary class="muted sub">查看引擎日志</summary><pre class="run-log" style="white-space:pre-wrap;max-height:220px">${mEsc(mJob.logTail)}</pre></details>` : ''}
  </div>`;
}
function mediaImageHtml() {
  mEnsureLoaded();
  const s = mSettings || {};
  return `<div class="page"><h1>画图</h1>
    <div class="muted sub" style="margin:-6px 0 12px">图片由本机 stable-diffusion.cpp 生成。模型权重由你选择；首次准备需要从模型发布页下载权重文件。</div>
    ${mEngineCard()}
    <div class="card"><h2>模型与提示词</h2>
      ${mField('图片模型（单文件 checkpoint 或 Z-Image diffusion）', 'imageModel', 'image_model')}
      ${mField('图片 VAE（Z-Image / Flux 分离式模型，单文件模型留空）', 'imageVae', 'image_vae')}
      ${mField('图片 LLM 文本编码器（Z-Image 的 Qwen3-4B，单文件模型留空）', 'imageTextEncoder', 'image_text_encoder')}
      <div class="muted sub" style="margin:-4px 0 10px">单文件 SD/SDXL checkpoint 只需选择“图片模型”。Z-Image 等分离式模型需要同时选择 diffusion、VAE 和 LLM 文本编码器；三项都选齐后会自动使用 --diffusion-model / --vae / --llm。</div>
      <div class="form-row"><label>正向提示词</label><textarea id="media-image-prompt" rows="3" placeholder="描述你想生成的画面"></textarea></div>
      <div class="form-row"><label>反向提示词（可选）</label><textarea id="media-image-negative" rows="2" placeholder="不希望出现的元素"></textarea></div>
      <div class="media-param-grid">
        <label>宽度<input id="media-image-width" type="number" min="256" max="1024" step="16" value="${Number(s.width) || 512}"></label>
        <label>高度<input id="media-image-height" type="number" min="256" max="1024" step="16" value="${Number(s.height) || 512}"></label>
        <label>步数<input id="media-image-steps" type="number" min="1" max="150" value="${Number(s.imageSteps) || 20}"></label>
        <label>CFG Scale<input id="media-image-cfg" type="number" min="0" max="30" step="0.5" value="${Number(s.imageCfgScale) || 7}"></label>
      </div>
      <div class="dl-actions"><button class="btn primary" data-media-action="generate-image" ${mJob.state === 'running' ? 'disabled' : ''}>开始生成</button><span class="muted sub">输出保存在 LocalRun 应用数据目录下的 media/output</span></div>
    </div>
    ${mJobCard()}
  </div>`;
}
function mediaVideoHtml() {
  mEnsureLoaded();
  const s = mSettings || {};
  const kind = s.videoModelKind || 'wan21_t2v';
  const kinds = [
    ['wan21_t2v', 'Wan2.1 T2V（1.3B / 14B）'],
    ['wan21_i2v', 'Wan2.1 I2V（14B，需 CLIP Vision + 首帧）'],
    ['wan22_ti2v', 'Wan2.2 TI2V 5B（wan2.2 VAE，无需 CLIP Vision）'],
    ['wan22_a14b', 'Wan2.2 A14B（LowNoise + HighNoise）'],
  ];
  return `<div class="page"><h1>视频</h1>
    <div class="muted sub" style="margin:-6px 0 12px">本地 Wan 视频生成。需要兼容的扩散模型、VAE 和 T5XXL/UMT5 权重；视频模型占用空间和内存较大，实际速度取决于 Mac 芯片与模型。</div>
    <div class="muted sub" style="margin:-6px 0 12px">模型组合和示例参数见 <a href="https://github.com/leejet/stable-diffusion.cpp/blob/master/docs/wan.md" target="_blank" rel="noreferrer">stable-diffusion.cpp Wan 文档</a>；准备权重后在下方分别选择文件。</div>
    ${mEngineCard()}
    <div class="card"><h2>模型与提示词</h2>
      <div class="form-row"><label>视频模型模式</label>
        <select id="media-video-kind">${kinds.map(([value, label]) => `<option value="${value}" ${kind === value ? 'selected' : ''}>${mEsc(label)}</option>`).join('')}</select>
        <div class="muted sub">Wan2.1 I2V 必须提供 CLIP Vision 与首帧；Wan2.2 A14B 必须同时提供 LowNoise 与 HighNoise 两个扩散模型；Wan2.2 TI2V 5B 使用 wan2.2 VAE 且不需要 CLIP Vision。</div>
      </div>
      ${mField('Wan 视频扩散模型（A14B 选 LowNoise）', 'videoDiffusion', 'video_diffusion')}
      ${mField('Wan2.2 A14B HighNoise 扩散模型（仅 A14B 需要）', 'videoHighNoiseDiffusion', 'video_high_noise_diffusion')}
      ${mField('VAE', 'videoVae', 'video_vae')}
      ${mField('T5XXL / UMT5 文本编码器', 'videoT5xxl', 'video_t5xxl')}
      ${mField('CLIP Vision（仅 Wan2.1 I2V 需要）', 'videoClipVision', 'video_clip_vision')}
      ${mField('首帧图片（Wan2.1 I2V 必填；TI2V/A14B 可选）', 'inputImage', 'input_image')}
      <div class="dl-actions" style="margin:-6px 0 12px"><button class="btn mini" data-media-action="clear-input">清除首帧</button></div>
      <div class="form-row"><label>提示词</label><textarea id="media-video-prompt" rows="3" placeholder="描述视频内容、镜头运动与动作变化"></textarea></div>
      <div class="media-param-grid">
        <label>宽度<input id="media-video-width" type="number" min="256" max="1024" step="16" value="${Number(s.width) || 832}"></label>
        <label>高度<input id="media-video-height" type="number" min="256" max="1024" step="16" value="${Number(s.height) || 480}"></label>
        <label>帧数<select id="media-video-frames">${[33, 77, 81].map((v) => `<option value="${v}" ${Number(s.videoFrames) === v ? 'selected' : ''}>${v} 帧</option>`).join('')}</select></label>
        <label>步数<input id="media-video-steps" type="number" min="1" max="150" value="${Number(s.videoSteps) || 20}"></label>
        <label>CFG Scale<input id="media-video-cfg" type="number" min="0" max="30" step="0.5" value="${Number(s.videoCfgScale) || 6}"></label>
      </div>
      <div class="dl-actions"><button class="btn primary" data-media-action="generate-video" ${mJob.state === 'running' ? 'disabled' : ''}>开始生成</button><span class="muted sub">输出为 WebM，保存于 media/output；Wan2.2 TI2V 5B 在 Apple Silicon 上会自动把 VAE 放到 CPU，速度较慢</span></div>
    </div>
    ${mJobCard()}
  </div>`;
}

function mEnsureLoaded() {
  if (mSettings) return Promise.resolve(mSettings);
  if (mLoadPromise) return mLoadPromise;
  mLoading = true;
  mLoadPromise = Promise.all([
    mInvoke('get_media_settings'),
    mInvoke('get_media_engine_status'),
    mInvoke('get_media_job_status'),
  ]).then(([settings, engine, job]) => {
    mSettings = settings;
    mEngine = engine;
    mJob = job;
    if (page === 'image' || page === 'video') render();
    return settings;
  }).catch((e) => {
    toast('读取本地媒体设置失败：' + e, true);
    return null;
  }).finally(() => {
    mLoading = false;
    mLoadPromise = null;
  });
  return mLoadPromise;
}
async function mSaveSettings() {
  mSettings = await mInvoke('save_media_settings', { settings: mSettings });
}
async function mPollJob() {
  if (page !== 'image' && page !== 'video') return;
  try {
    const next = await mInvoke('get_media_job_status');
    const changed = !mJob || next.state !== mJob.state || next.jobId !== mJob.jobId || next.outputPath !== mJob.outputPath;
    mJob = next;
    if (changed) render();
    else {
      const message = document.getElementById('media-job-message');
      if (message) message.textContent = next.message || '';
    }
  } catch (_) { /* 页面离开或应用正在关闭时忽略 */ }
}
async function mHandleAction(button) {
  const action = button.dataset.mediaAction;
  try {
    await mEnsureLoaded();
    if (!mSettings) throw new Error('本地媒体设置未能加载，请检查应用数据目录后重试');
    if (action === 'pick-engine') {
      const path = await mInvoke('pick_sd_cli');
      if (path) mSettings.sdCliPath = path;
      mEngine = await mInvoke('get_media_engine_status');
      render();
    } else if (action === 'pick-file') {
      const kind = button.dataset.kind;
      const path = await mInvoke('pick_media_file', { kind });
      if (path) {
        const fields = {
          image_model: 'imageModel',
          image_vae: 'imageVae',
          image_text_encoder: 'imageTextEncoder',
          video_diffusion: 'videoDiffusion',
          video_high_noise_diffusion: 'videoHighNoiseDiffusion',
          video_vae: 'videoVae',
          video_t5xxl: 'videoT5xxl',
          video_clip_vision: 'videoClipVision',
          input_image: 'inputImage',
        };
        mSettings[fields[kind]] = path;
        await mSaveSettings();
        render();
      }
    } else if (action === 'clear-input') {
      mSettings.inputImage = null;
      await mSaveSettings();
      render();
    } else if (action === 'generate-image') {
      if (!mEngine || !mEngine.available) throw new Error('请先选择可执行的本地 sd-cli');
      const prompt = document.getElementById('media-image-prompt').value.trim();
      const negativePrompt = document.getElementById('media-image-negative').value.trim();
      const width = Number(document.getElementById('media-image-width').value);
      const height = Number(document.getElementById('media-image-height').value);
      const steps = Number(document.getElementById('media-image-steps').value);
      const cfgScale = Number(document.getElementById('media-image-cfg').value);
      mSettings.width = width; mSettings.height = height; mSettings.imageSteps = steps; mSettings.imageCfgScale = cfgScale;
      await mSaveSettings();
      mJob = await mInvoke('generate_image', { prompt, negativePrompt, width, height, steps, cfgScale });
      render();
    } else if (action === 'generate-video') {
      if (!mEngine || !mEngine.available) throw new Error('请先选择可执行的本地 sd-cli');
      const prompt = document.getElementById('media-video-prompt').value.trim();
      const width = Number(document.getElementById('media-video-width').value);
      const height = Number(document.getElementById('media-video-height').value);
      const frames = Number(document.getElementById('media-video-frames').value);
      const videoSteps = Number(document.getElementById('media-video-steps').value);
      const videoCfgScale = Number(document.getElementById('media-video-cfg').value);
      const videoModelKind = document.getElementById('media-video-kind').value;
      mSettings.width = width;
      mSettings.height = height;
      mSettings.videoFrames = frames;
      mSettings.videoSteps = videoSteps;
      mSettings.videoCfgScale = videoCfgScale;
      mSettings.videoModelKind = videoModelKind;
      await mSaveSettings();
      mJob = await mInvoke('generate_video', { prompt, width, height, frames, inputImage: mSettings.inputImage || null });
      render();
    } else if (action === 'cancel' && button.dataset.job) {
      mJob = await mInvoke('cancel_media_job', { jobId: button.dataset.job });
      render();
    } else if (action === 'reveal-output' && button.dataset.output) {
      await mInvoke('reveal_media_output', { path: button.dataset.output });
    }
  } catch (e) {
    toast('媒体任务失败：' + e, true);
  }
}

document.addEventListener('click', (event) => {
  const button = event.target.closest('[data-media-action]');
  if (button) mHandleAction(button);
});

mRefreshTimer = setInterval(mPollJob, 1200);

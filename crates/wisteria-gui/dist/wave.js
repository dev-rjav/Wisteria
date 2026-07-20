// Flow Bar wave engine — magenta sine × cos strands, ported from the Flow App design.
// Exposes window.WisteriaWave with setMode(mode) where mode is idle|listening|processing.
(function () {
  const canvas = document.getElementById('dock-canvas');
  const ctx = canvas.getContext('2d');
  let mode = 'idle';
  let phase = 0, energy = 0, target = 0, burst = 0;
  const TAU = Math.PI * 2;

  function draw() {
    const dpr = window.devicePixelRatio || 1;
    const w = canvas.clientWidth, h = canvas.clientHeight;
    if (w && h) {
      if (canvas.width !== Math.round(w * dpr) || canvas.height !== Math.round(h * dpr)) {
        canvas.width = Math.round(w * dpr);
        canvas.height = Math.round(h * dpr);
      }
      ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
      ctx.clearRect(0, 0, w, h);

      let speed;
      if (mode === 'listening') {
        speed = 0.09;
        burst -= 1;
        if (burst <= 0) { target = 0.4 + Math.random() * 0.6; burst = 6 + Math.floor(Math.random() * 22); }
      } else if (mode === 'processing') {
        speed = 0.035; target = 0.32;
      } else {
        speed = 0.02; target = 0.06;
      }
      phase += speed;
      energy += (target - energy) * 0.12;

      const midY = h / 2, baseAmp = h / 2 - 8, amp = baseAmp * Math.min(energy, 1.1);
      const strand = (freq, off, mul, fn, color, glow, lw) => {
        ctx.beginPath();
        for (let x = 0; x <= w; x += 2) {
          const t = x / w;
          const env = Math.pow(Math.sin(Math.PI * t), 0.85);
          const y = midY + amp * mul * fn(freq * t * TAU + phase + off) * env;
          x === 0 ? ctx.moveTo(x, y) : ctx.lineTo(x, y);
        }
        const g = ctx.createLinearGradient(0, 0, w, 0);
        g.addColorStop(0, 'rgba(0,0,0,0)');
        g.addColorStop(0.15, color);
        g.addColorStop(0.85, color);
        g.addColorStop(1, 'rgba(0,0,0,0)');
        ctx.strokeStyle = g;
        ctx.lineWidth = lw;
        ctx.lineCap = 'round';
        ctx.shadowColor = glow;
        ctx.shadowBlur = mode === 'listening' ? 13 : 7;
        ctx.stroke();
      };
      strand(2.4, 0.9, 0.72, Math.cos, 'rgba(217,70,239,0.75)', 'rgba(217,70,239,0.6)', 2.2);
      strand(3.1, 0.0, 1.0, Math.sin, 'rgba(240,171,252,0.95)', 'rgba(232,121,249,0.7)', 2.4);
      ctx.shadowBlur = 0;
    }
    requestAnimationFrame(draw);
  }
  requestAnimationFrame(draw);

  window.WisteriaWave = { setMode(m) { mode = m; } };
})();

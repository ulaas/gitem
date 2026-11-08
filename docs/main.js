// Reveal on scroll, a solid header once you've scrolled, a copy button, and a gentle tilt
// on the screenshots under the pointer. Everything here is optional: without JavaScript, or
// with reduced motion, the page shows all of its content at once.
(() => {
  const reduced = window.matchMedia('(prefers-reduced-motion: reduce)').matches;

  // Reveal each section's parts as they come into view.
  const items = document.querySelectorAll('.reveal');
  if (reduced || !('IntersectionObserver' in window)) {
    items.forEach((el) => el.classList.add('is-visible'));
  } else {
    const seen = new IntersectionObserver((entries) => {
      for (const e of entries) {
        if (e.isIntersecting) {
          e.target.classList.add('is-visible');
          seen.unobserve(e.target);
        }
      }
    }, { rootMargin: '0px 0px -12% 0px', threshold: 0.12 });
    items.forEach((el) => seen.observe(el));
  }

  // The header turns solid once the hero has scrolled a little.
  const nav = document.querySelector('.nav');
  const onScroll = () => nav.classList.toggle('is-solid', window.scrollY > 40);
  onScroll();
  window.addEventListener('scroll', onScroll, { passive: true });

  // Copy the build commands.
  document.querySelectorAll('.copy').forEach((btn) => {
    btn.addEventListener('click', async () => {
      const text = document.getElementById(btn.dataset.copy).textContent;
      try {
        await navigator.clipboard.writeText(text);
        btn.textContent = 'Copied';
      } catch {
        btn.textContent = 'Select and copy';
      }
      setTimeout(() => { btn.textContent = 'Copy'; }, 1800);
    });
  });

  // A small tilt toward the pointer, for mice only.
  if (!reduced && window.matchMedia('(hover: hover) and (pointer: fine)').matches) {
    document.querySelectorAll('.feature-shot .bezel, .hero-shot .bezel').forEach((bezel) => {
      bezel.addEventListener('pointermove', (ev) => {
        const r = bezel.getBoundingClientRect();
        const x = (ev.clientX - r.left) / r.width - 0.5;
        const y = (ev.clientY - r.top) / r.height - 0.5;
        bezel.style.transform = `rotateX(${(-y * 4).toFixed(2)}deg) rotateY(${(x * 5).toFixed(2)}deg)`;
      });
      bezel.addEventListener('pointerleave', () => { bezel.style.transform = ''; });
    });
  }
})();

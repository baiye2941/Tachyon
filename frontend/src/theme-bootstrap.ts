;(function () {
  const raw = localStorage.getItem('tachyon-theme')
  const theme = raw === 'dark' || raw === 'light' ? raw : 'dark'
  document.documentElement.setAttribute('data-theme', theme)
})()

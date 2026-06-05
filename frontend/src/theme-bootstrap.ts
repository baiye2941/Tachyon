;(function () {
  const theme = localStorage.getItem('tachyon-theme') || 'dark'
  document.documentElement.setAttribute('data-theme', theme)
})()

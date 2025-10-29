async function enableSidePanel() {
  try {
    await chrome.sidePanel.setOptions({ path: 'panel.html', enabled: true });
  } catch (error) {
    console.error('Failed to enable side panel', error);
  }
}

chrome.runtime.onInstalled.addListener(() => {
  enableSidePanel();
});

chrome.runtime.onStartup.addListener(() => {
  enableSidePanel();
});

chrome.action.onClicked.addListener(async (tab) => {
  try {
    await chrome.sidePanel.open({ windowId: tab.windowId });
  } catch (error) {
    console.error('Unable to open side panel', error);
  }
});

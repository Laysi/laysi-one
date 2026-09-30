import { StrictMode } from 'react';
import { createRoot } from 'react-dom/client';
import { App } from './App';
import { uploadLinkSecret } from './nav';
import { UploadLink } from './screens/UploadLink';
import './styles/classical.css';

const root = document.getElementById('root');
if (!root) throw new Error('頁面無法載入，請重新整理或洽系統管理員');

const uploadSecret = uploadLinkSecret(window.location.pathname);

createRoot(root).render(
  <StrictMode>{uploadSecret ? <UploadLink secret={uploadSecret} /> : <App />}</StrictMode>,
);

if (import.meta.env.PROD && 'serviceWorker' in navigator) {
  window.addEventListener('load', () => {
    void navigator.serviceWorker.register('/sw.js');
  });
}

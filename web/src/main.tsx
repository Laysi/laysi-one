import { StrictMode } from 'react';
import { createRoot } from 'react-dom/client';
import { App } from './App';
import './styles/classical.css';

const root = document.getElementById('root');
if (!root) throw new Error('頁面無法載入，請重新整理或洽系統管理員');

createRoot(root).render(
  <StrictMode>
    <App />
  </StrictMode>,
);

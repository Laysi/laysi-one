import { StrictMode } from 'react';
import { createRoot } from 'react-dom/client';
import { App } from './App';
import './styles/classical.css';

const root = document.getElementById('root');
if (!root) throw new Error('找不到 #root，index.html 可能被改壞了');

createRoot(root).render(
  <StrictMode>
    <App />
  </StrictMode>,
);

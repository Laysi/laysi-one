import { createContext, useCallback, useContext, useEffect, useState } from 'react';
import { setUnauthorizedHandler, type ApiError } from './api/client';

export interface SessionUser {
  id: number;
  username: string;
  display_name: string;
  email: string;
  role: string;
  role_name: string;
  permissions: string[];
  employee_id: number | null;
  must_change_password: boolean;
}

export class AuthFailed extends Error {
  constructor(public readonly error: ApiError) {
    super(error.message);
    this.name = 'AuthFailed';
  }
}

export const NO_USER_ACCOUNT = 'no_user_account';

export interface SetupState {
  bootstrap_available: boolean;
  token_required: boolean;
}

export interface BootstrapInput {
  username: string;
  display_name: string;
  email: string;
  password: string;
  setup_token: string;
}

const CLOSED_SETUP: SetupState = { bootstrap_available: false, token_required: false };

export type AuthStatus = 'loading' | 'signed-out' | 'signed-in';

export interface AuthValue {
  status: AuthStatus;
  user: SessionUser | null;
  oauthEnabled: boolean;
  setup: SetupState;
  signIn: (username: string, password: string) => Promise<SessionUser>;
  signOut: () => Promise<void>;
  changePassword: (currentPassword: string, newPassword: string) => Promise<SessionUser>;
  bootstrap: (input: BootstrapInput) => Promise<SessionUser>;
}

export const OAUTH_START_URL = '/v1/auth/oauth/start';

const AuthContext = createContext<AuthValue | null>(null);

export const AuthProvider = AuthContext.Provider;

export function useAuth(): AuthValue {
  const value = useContext(AuthContext);
  if (!value) throw new Error('useAuth 必須在 AuthProvider 之內使用');
  return value;
}

export function useCurrentUser(): SessionUser {
  const { user } = useAuth();
  if (!user) throw new Error('useCurrentUser 只能在已登入的畫面裡使用');
  return user;
}

interface Envelope<T> {
  ok?: boolean;
  error?: ApiError;
  user?: T;
  oauth?: { enabled: boolean };
  setup?: SetupState;
}

function landOnStart(): void {
  if (window.location.pathname !== '/') {
    window.history.replaceState({}, '', '/');
  }
}

async function readEnvelope(res: Response): Promise<Envelope<SessionUser>> {
  const body: unknown = await res.json().catch(() => null);
  if (body && typeof body === 'object') return body as Envelope<SessionUser>;
  return {};
}

function failure(res: Response, envelope: Envelope<SessionUser>): AuthFailed {
  return new AuthFailed(
    envelope.error ?? { code: 'network', message: `伺服器回應異常（HTTP ${res.status}）` },
  );
}

const JSON_HEADERS: HeadersInit = { 'content-type': 'application/json' };

async function fetchMe(): Promise<{
  user: SessionUser | null;
  oauthEnabled: boolean;
  setup: SetupState;
}> {
  const res = await fetch('/v1/auth/me', { headers: JSON_HEADERS });
  const envelope = await readEnvelope(res);
  const oauthEnabled = envelope.oauth?.enabled ?? false;
  const setup = envelope.setup ?? CLOSED_SETUP;
  if (res.status === 401) return { user: null, oauthEnabled, setup };
  if (!res.ok || !envelope.user) throw failure(res, envelope);
  return { user: envelope.user, oauthEnabled, setup };
}

export function useAuthSession(): AuthValue {
  const [status, setStatus] = useState<AuthStatus>('loading');
  const [user, setUser] = useState<SessionUser | null>(null);
  const [oauthEnabled, setOauthEnabled] = useState(false);
  const [setup, setSetup] = useState<SetupState>(CLOSED_SETUP);

  useEffect(() => {
    setUnauthorizedHandler(() => {
      setUser(null);
      setStatus('signed-out');
    });
    return () => setUnauthorizedHandler(null);
  }, []);

  useEffect(() => {
    let live = true;
    fetchMe()
      .then((session) => {
        if (!live) return;
        setOauthEnabled(session.oauthEnabled);
        setSetup(session.setup);
        setUser(session.user);
        setStatus(session.user ? 'signed-in' : 'signed-out');
      })
      .catch(() => {
        if (!live) return;
        setStatus('signed-out');
      });
    return () => {
      live = false;
    };
  }, []);

  const signIn = useCallback(async (username: string, password: string) => {
    const res = await fetch('/v1/auth/login', {
      method: 'POST',
      headers: JSON_HEADERS,

      body: JSON.stringify({ username, password }),
    });
    const envelope = await readEnvelope(res);
    if (!res.ok || !envelope.user) throw failure(res, envelope);
    landOnStart();
    setUser(envelope.user);
    setStatus('signed-in');
    return envelope.user;
  }, []);

  const signOut = useCallback(async () => {
    try {
      await fetch('/v1/auth/logout', { method: 'POST', headers: JSON_HEADERS });
    } finally {
      setUser(null);
      setStatus('signed-out');
    }
  }, []);

  const changePassword = useCallback(async (currentPassword: string, newPassword: string) => {
    const res = await fetch('/v1/auth/change-password', {
      method: 'POST',
      headers: JSON_HEADERS,
      body: JSON.stringify({ current_password: currentPassword, new_password: newPassword }),
    });
    const envelope = await readEnvelope(res);
    if (!res.ok || !envelope.user) throw failure(res, envelope);
    setUser(envelope.user);
    setStatus('signed-in');
    return envelope.user;
  }, []);

  const bootstrap = useCallback(async (input: BootstrapInput) => {
    const res = await fetch('/v1/auth/bootstrap', {
      method: 'POST',
      headers: JSON_HEADERS,
      body: JSON.stringify(input),
    });
    const envelope = await readEnvelope(res);
    if (!res.ok || !envelope.user) throw failure(res, envelope);
    setSetup(CLOSED_SETUP);
    landOnStart();
    setUser(envelope.user);
    setStatus('signed-in');
    return envelope.user;
  }, []);

  return { status, user, oauthEnabled, setup, signIn, signOut, changePassword, bootstrap };
}

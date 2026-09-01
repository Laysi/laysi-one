import { createContext, useCallback, useContext, useEffect, useState } from 'react';
import {
  authEnvelope,
  NO_BRAND,
  noteServerTime,
  setUnauthorizedHandler,
  SHAPE_MISMATCH,
  type ApiError,
  type AuthEnvelope,
  type Brand,
  type OauthState,
  type SessionUser,
  type SetupState,
} from './api/client';
import { consentRequested } from './nav';

export type { Brand, OauthState, SessionUser, SetupState };

export class AuthFailed extends Error {
  constructor(public readonly error: ApiError) {
    super(error.message);
    this.name = 'AuthFailed';
  }
}

export const NO_USER_ACCOUNT = 'no_user_account';

export interface BootstrapInput {
  password: string;
  setup_token: string;
}

const CLOSED_SETUP: SetupState = { bootstrap_available: false, token_required: false };

const OAUTH_OFF: OauthState = {
  enabled: false,
  label: '',
  domain: null,
  creates_accounts: false,
  password_login_disabled: false,
};

export type AuthStatus = 'loading' | 'signed-out' | 'signed-in' | 'unreachable';

const UNREACHABLE: ApiError = {
  code: 'network',
  message: '無法連線到伺服器，請確認伺服器已啟動。',
};

export interface AuthValue {
  status: AuthStatus;
  problem: ApiError | null;
  user: SessionUser | null;
  oauth: OauthState;
  setup: SetupState;
  brand: Brand;
  signIn: (username: string, password: string) => Promise<SessionUser>;
  signOut: () => Promise<void>;
  changePassword: (newPassword: string) => Promise<SessionUser>;
  bootstrap: (input: BootstrapInput) => Promise<SessionUser>;
}

export function oauthStartUrl(): string {
  const base = '/v1/auth/oauth/start';
  if (!consentRequested()) return base;
  const here = window.location.pathname + window.location.search;
  return `${base}?return_to=${encodeURIComponent(here)}`;
}

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

function landOnStart(): void {
  if (window.location.pathname !== '/' && !consentRequested()) {
    window.history.replaceState({}, '', '/');
  }
}

async function readEnvelope(res: Response): Promise<AuthEnvelope> {
  const body: unknown = await res.json().catch(() => null);
  if (body === null) return {};
  const read = authEnvelope.safeParse(body);
  return read.success ? read.data : { error: SHAPE_MISMATCH };
}

function failure(res: Response, read: AuthEnvelope): AuthFailed {
  return new AuthFailed(
    read.error ?? { code: 'network', message: `伺服器回應異常（HTTP ${res.status}）` },
  );
}

const JSON_HEADERS: HeadersInit = { 'content-type': 'application/json' };

async function fetchMe(): Promise<{
  user: SessionUser | null;
  oauth: OauthState;
  setup: SetupState;
  brand: Brand;
}> {
  const res = await fetch('/v1/auth/me', { headers: JSON_HEADERS });
  noteServerTime(res);
  const envelope = await readEnvelope(res);
  const oauth: OauthState = { ...OAUTH_OFF, ...envelope.oauth };
  const setup = envelope.setup ?? CLOSED_SETUP;
  const brand = envelope.brand ?? NO_BRAND;
  if (res.status === 401) return { user: null, oauth, setup, brand };
  if (!res.ok || !envelope.user) throw failure(res, envelope);
  return { user: envelope.user, oauth, setup, brand };
}

export function useAuthSession(): AuthValue {
  const [status, setStatus] = useState<AuthStatus>('loading');
  const [problem, setProblem] = useState<ApiError | null>(null);
  const [user, setUser] = useState<SessionUser | null>(null);
  const [oauth, setOauth] = useState<OauthState>(OAUTH_OFF);
  const [setup, setSetup] = useState<SetupState>(CLOSED_SETUP);
  const [brand, setBrand] = useState<Brand>(NO_BRAND);

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
        setOauth(session.oauth);
        setSetup(session.setup);
        setBrand(session.brand);
        setUser(session.user);
        setStatus(session.user ? 'signed-in' : 'signed-out');
      })
      .catch((err: unknown) => {
        if (!live) return;
        setProblem(err instanceof AuthFailed ? err.error : UNREACHABLE);
        setStatus('unreachable');
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

  const changePassword = useCallback(async (newPassword: string) => {
    const res = await fetch('/v1/auth/change-password', {
      method: 'POST',
      headers: JSON_HEADERS,
      body: JSON.stringify({ new_password: newPassword }),
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

  return {
    status,
    problem,
    user,
    oauth,
    setup,
    brand,
    signIn,
    signOut,
    changePassword,
    bootstrap,
  };
}

/**
 * Sign-in state.
 *
 * Identity has one source: the server-side session. The browser holds only
 * an HttpOnly cookie, which the frontend cannot read and must not try to;
 * no line here touches localStorage or sessionStorage, and a password never
 * outlives the request that submits it.
 *
 * "Who am I" is therefore always asked, never remembered: `GET /v1/auth/me`
 * once per load, and any operation hitting a 401 falls back to the login
 * screen. The frontend does not compute session expiry; both the idle and
 * absolute limits live on the server, and guessing would only be wrong.
 */

import { createContext, useCallback, useContext, useEffect, useState } from 'react';
import { setUnauthorizedHandler, type ApiError } from './api/client';

/** A signed-in account, matching the `user` object of `GET /v1/auth/me`. */
export interface SessionUser {
  id: number;
  username: string;
  display_name: string;
  email: string;
  /**
   * Role code (`boss`, `acct`, `staff`, ...). Permissions are checked
   * against it by the backend; the frontend never decides from it.
   */
  role: string;
  /**
   * Display name of the role, supplied by the backend.
   *
   * The full role list lives in `list_roles`, which staff may not call, so
   * a person's own role name comes straight from `/v1/auth/me`. The
   * frontend must not keep a code-to-name table, which would copy the
   * permission model and eventually disagree with it.
   */
  role_name: string;
  /** Which roster row this links to. null means no link, and the five
   *  self-service operations return an error. */
  employee_id: number | null;
  /** The administrator's initial password has not been changed yet, and the
   *  main screen stays out of reach until it is. */
  must_change_password: boolean;
}

/** A failure from an auth endpoint. `code` comes from the backend and tells
 *  the screen which message to show. */
export class AuthFailed extends Error {
  constructor(public readonly error: ApiError) {
    super(error.message);
    this.name = 'AuthFailed';
  }
}

/** The system has no accounts at all. Not a wrong password: guide the user
 *  to create one. */
export const NO_USER_ACCOUNT = 'no_user_account';

export type AuthStatus = 'loading' | 'signed-out' | 'signed-in';

export interface AuthValue {
  status: AuthStatus;
  /** Never null when `status` is `signed-in`. */
  user: SessionUser | null;
  /** Whether this deployment has OIDC configured. Without it, no button
   *  should be drawn that would 404. */
  oauthEnabled: boolean;
  signIn: (username: string, password: string) => Promise<SessionUser>;
  signOut: () => Promise<void>;
  changePassword: (currentPassword: string, newPassword: string) => Promise<SessionUser>;
}

/** OAuth is handled end to end by the backend: 302 to the IdP, then 302
 *  back to `/` after validation. The frontend never sees the code. */
export const OAUTH_START_URL = '/v1/auth/oauth/start';

const AuthContext = createContext<AuthValue | null>(null);

export const AuthProvider = AuthContext.Provider;

export function useAuth(): AuthValue {
  const value = useContext(AuthContext);
  if (!value) throw new Error('useAuth 必須在 AuthProvider 之內使用');
  return value;
}

/**
 * The current user. Only callable from signed-in screens; `App` does not
 * render them otherwise.
 *
 * For screens that need to act in the user's name (issuing a token records
 * who issued it, for instance). It is not for deciding who may do what;
 * that is the backend's job.
 */
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

/**
 * The current session. Signed out is a 401, which is a normal result, not
 * an error.
 *
 * The 401 response carries `oauth` as well, so the login screen does not
 * have to probe `/v1/auth/oauth/start` to know whether to draw "sign in
 * with OAuth". Such a probe would leave a PKCE entry on the server that
 * nobody completes, and whether this deployment has OAuth is not a secret.
 */
async function fetchMe(): Promise<{ user: SessionUser | null; oauthEnabled: boolean }> {
  const res = await fetch('/v1/auth/me', { headers: JSON_HEADERS });
  const envelope = await readEnvelope(res);
  const oauthEnabled = envelope.oauth?.enabled ?? false;
  if (res.status === 401) return { user: null, oauthEnabled };
  if (!res.ok || !envelope.user) throw failure(res, envelope);
  return { user: envelope.user, oauthEnabled };
}

/**
 * Source of the sign-in state. Called once for the whole application, with
 * the value passed down by `AuthProvider`.
 */
export function useAuthSession(): AuthValue {
  const [status, setStatus] = useState<AuthStatus>('loading');
  const [user, setUser] = useState<SessionUser | null>(null);
  const [oauthEnabled, setOauthEnabled] = useState(false);

  // Any operation hitting a 401 falls back to the login screen. Installed
  // once.
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
      // The password exists only in this request body. It is never kept in
      // state or written to any storage.
      body: JSON.stringify({ username, password }),
    });
    const envelope = await readEnvelope(res);
    if (!res.ok || !envelope.user) throw failure(res, envelope);
    setUser(envelope.user);
    setStatus('signed-in');
    return envelope.user;
  }, []);

  const signOut = useCallback(async () => {
    // A failed revocation still returns to the login screen: leaving someone
    // on a screen that looks signed in is worse.
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

  return { status, user, oauthEnabled, signIn, signOut, changePassword };
}

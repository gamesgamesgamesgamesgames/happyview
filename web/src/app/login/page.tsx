"use client";

import { useEffect } from "react";
import { useRouter, useSearchParams } from "next/navigation";
import { LoginForm } from "@/components/login-form";
import { useAuth } from "@/lib/auth-context";
import { loginErrorMessage } from "./login-error";

export default function LoginPage() {
  const { did } = useAuth();
  const router = useRouter();
  const searchParams = useSearchParams();
  const errorMessage = loginErrorMessage(searchParams);

  useEffect(() => {
    if (did) router.replace("/dashboard");
  }, [did, router]);

  if (did) return null;

  return (
    <div className="bg-background flex min-h-svh flex-col items-center justify-center gap-6 p-6 md:p-10">
      <div className="w-full max-w-sm">
        <LoginForm externalError={errorMessage} />
      </div>
    </div>
  );
}

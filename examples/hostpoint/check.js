// Logged in? The customer panel embeds a CSRF token in every page; when the
// session is gone, /customer/Index redirects to the login page instead.
return !!document.querySelector('input[name="csrf_token"]');

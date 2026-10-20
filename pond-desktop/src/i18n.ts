import i18n from 'i18next';
import { initReactI18next } from 'react-i18next';

void i18n.use(initReactI18next).init({
  lng: 'en', fallbackLng: 'en', interpolation: { escapeValue: false },
  resources: { en: { translation: { remote: {
    title: 'Remote access',
    recoveryTitle: 'Phone recovery requests',
    recoveryDescription: 'After pairing the phone locally again, check that the name and key below match what the phone shows before approving. Approval expires after two minutes and must be completed by that phone on your home network.',
    reviewDevice: 'Phone: {{name}}', reviewUnnamed: 'Phone: name unavailable',
    reviewKey: 'Key begins: {{key}}',
    reviewCode: 'Request: {{id}}', reviewApprove: 'Approve this phone recovery', reviewApproved: 'Approved; waiting for phone',
    reviewFailed: 'Recovery requests could not be loaded or approved. Check the local connection and retry.',
    retired_capacity: 'Approved, but the coordination service refused it: this household has replaced as many phones as it can keep track of for now. Each replaced phone is forgotten a day after it last appeared, so try this recovery again tomorrow.',
    description: 'Reach this Pond from outside your home. Until you turn this on, your Pond contacts no coordination service at all.',
    identity: 'Prepare household identity', household: 'Household identifier', publicKey: 'Enrollment public key',
    provision: 'Give this public identity to your operator. Keep the private identity and its backup on your Pond.',
    coordinator: 'Headscale HTTPS origin', enrollment: 'Enrollment HTTPS origin',
    toggleLabel: 'Reach this Pond from outside your home',
    advanced: 'Use my own coordination service',
    advancedDescription: 'Leave these empty to use the hosted service. Set both to run your own; setting only one is rejected.',
    enable: 'Enable remote access', disable: 'Keep local only',
    local: 'Remote access is disabled.', connecting: 'Registering this Pond with the coordinator.',
    enabled: 'Remote access is enabled. Paired phones can now request enrollment on your LAN.',
    failed: 'Remote access did not complete. Check the local server logs and pilot provisioning before retrying.',
    invite: 'Invite', inviteHint: 'Needed once, the first time this Pond joins a coordination service. Whoever runs the service gives it to you.',
    invite_required: 'This coordination service admits new households by invite. Enter the invite you were given and try again.',
    invite_invalid: 'That invite was not recognised. Check it against the one you were given.',
    invite_expired: 'That invite has expired. Ask for a new one.',
    invite_used: 'That invite has already admitted another household. Ask for a new one.',
    provisioned: 'This Pond was provisioned when it was set up, so it joins the hosted coordination service without an invite. Device serial: {{serial}}',
    registered: 'This Pond\'s household is already registered with its coordination service, so it needs no invite.',
    device_certificate_invalid: 'The coordination service does not recognise this Pond\'s device certificate. Ask whoever runs it for an invite and enter it below.',
    device_revoked: 'This Pond\'s device certificate has been revoked. Ask whoever runs the coordination service for an invite and enter it below.',
    device_used: 'This Pond\'s device certificate has already admitted another household. Ask whoever runs the coordination service for an invite and enter it below.',
  }, signIn: {
    title: 'Sign in to this Pond',
    instructions: 'This browser has not been signed in. On the Pond, run this command and open the link it prints:',
    restart: 'The link stops working when the Pond restarts. Run the command again for a new one.',
    hostOnly: 'This browser is no longer signed in to the Pond, probably because it restarted. Run `pond-server dashboard` on the Pond and open the new link.',
    shellTitle: 'This app is not signed in to the Pond',
    shellMismatch: 'This app\'s sign-in does not match the Pond it is talking to. Restart the app, and if that does not help, check that it is pointed at the Pond you meant to use.',
  }, models: {
    download: {
      reconnecting: 'The connection dropped. Reconnecting (attempt {{attempt}} of {{max}})',
    },
    unavailable: {
      litert_library_missing: 'Needs the LiteRT-LM library, which this Pond does not have yet',
      other: 'This pond cannot run this model',
    },
  }, extensions: {
    noTools: 'Running, no assistant tools here',
    noToolsGeneric: 'This extension is running, but it gives the assistant nothing to use on this Pond.',
  }, memory: {
    kept: 'Kept',
    segment: {
      identity: 'About you', preference: 'Preference', correction: 'Correction', relationship: 'People',
      project: 'Project', routine: 'Habit', knowledge: 'Knowledge', context: 'Right now',
    },
  }, schedules: {
    date: 'Date', dateLabel: 'Run date',
    onceAt: 'Runs once on {{date}} at {{time}}',
    oncePast: 'That date and time has already passed. Pick a moment in the future.',
    unchanged: 'Nothing changed, so the schedule was left as it was.',
  }, pairing: {
    unavailable: 'HTTPS pairing is unavailable. Check the Pond server logs.',
    fingerprint: 'Public-key fingerprint', address: 'HTTPS address',
    manual: 'For manual pairing, enter this address, fingerprint, and pairing code on your phone while connected to the same LAN.',
  } } } },
});
export default i18n;

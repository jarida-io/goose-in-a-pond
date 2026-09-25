import i18n from 'i18next';
import { initReactI18next } from 'react-i18next';

void i18n.use(initReactI18next).init({
  lng: 'en', fallbackLng: 'en', interpolation: { escapeValue: false },
  resources: { en: { translation: { pairing: {
    unavailable: 'HTTPS pairing is unavailable. Check the Pond server logs.',
    fingerprint: 'Public-key fingerprint', address: 'HTTPS address',
    manual: 'For manual pairing, enter this address, fingerprint, and pairing code on your phone while connected to the same LAN.',
  } } } },
});
export default i18n;

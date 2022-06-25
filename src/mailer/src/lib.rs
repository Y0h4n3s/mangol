
const BOT_TOKEN: &str = "5542140231:AAHBAyDnQbK2Q44GoWaYDtxQaFogF0qMJA0";
pub fn send_text_with_content(content: String) -> bool {
   telegram_notifyrs::send_message(content, BOT_TOKEN, 359883518);
    true
}

#[cfg(test)]
mod tests {
    use crate::{ send_text_with_content};
    
    #[test]
    fn send_mail() {
        let mail_content = "hello old friend, welcome to the next level";
        assert_eq!(true,send_text_with_content(mail_content.to_string()));
        
    }
}
